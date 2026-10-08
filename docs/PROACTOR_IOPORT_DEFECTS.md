# Proactor IoPort Defects

Status: defects 1 and 2 **fixed 2026-09-08**; defect 3 (below) fixed 2026-10-05;
defects 4 (IOCP handle reuse) and 5 (IOCP deadlines on the 15.6 ms tick)
fixed 2026-10-08.
The profiling commits defects 4 and 5 cite are kept on GitHub branches
`iocp-handle-reuse` and `iocp-timers`; keep those branches while this
record cites them.
Originally: **both fixed 2026-09-08**, the same day they were found while
migrating `starlight` onto `Proactor<IoUringPort>`. This document is kept
as the record of what was wrong and why the fixes took the shape they
did; the "Suggested fix" sections below became the actual fixes.

Neither defect was hypothetical: the first silently broke starlight's
thermal socket on real hardware, and the second was the reason starlight
originally avoided `IoPort::accept` entirely.

**What changed**

| | fix |
| --- | --- |
| 1. reserved readiness tokens | `ProactorHandle::register_readable` rejects [`RESERVED_READINESS_TOKENS`] (`[1, 2]`) with `InvalidInput`, on every backend rather than only io_uring |
| 2. accept descriptor leak | `AcceptTransfer::peer` became the `PeerAddr` enum; `accept` now always returns the descriptor, whatever the address family |

**Verification** — all four backends, since the leak was in all four:

| backend | how |
| --- | --- |
| `KqueuePort` | 10/10 tests on macmini, including two new ones |
| `IoUringPort` | 11/11 tests on `dolores`, including two new ones |
| `EpollPort` | `clippy --target aarch64-linux-android -D warnings` |
| `IocpPort` | `clippy --target x86_64-pc-windows-msvc -D warnings` |

Plus the full CI-equivalent gate on `dolores` (`cargo fmt --check`, and
`clippy`/`test --workspace --all-targets --all-features` with
`PLATFORM_EXCLUDES`), all clean.

Six regression tests were added, two per Unix backend, and both fail
against the previous code.

**Two portability traps surfaced while fixing this**, both caught by
`dolores` after macmini was clean, and both worth remembering:

- `uring.rs` needed a `PeerAddr` import that macOS never compiles, so the
  local clippy run was clean and wrong;
- `sa_family_t` is `u16` on Linux and `u8` on macOS/BSD, and `c_char` is
  unsigned on aarch64 Linux but signed on darwin. There is no cast form
  clippy accepts on both — `as u16` trips `unnecessary_cast` on one,
  `u16::from` trips `useless_conversion` on the other.

---

## 1. `IoUringPort` readiness tokens collide with reserved `user_data`

**Severity:** silent no-op. No error, no log, nothing in `journalctl`.

**Affects:** `IoUringPort` only. `EpollPort` is already immune — see
"Suggested fix", which is just "do what epoll does".

### What happens

`proactor/src/uring.rs` reserves two `user_data` values:

```rust
const QUEUE_TOKEN: u64 = 1;
const WAKE_TOKEN: u64 = 2;
```

`poll()` dispatches on the raw CQE `user_data`, matching those two first,
then `token & IO_OP_TAG != 0` for `IoPort` operations, then falling
through to the readiness registry. Readiness tokens therefore share a
value space with `QUEUE_TOKEN` and `WAKE_TOKEN`.

`register_readable(fd, 1, handler)` returns `Ok(())`. The `PollAdd` is
submitted correctly. When it completes, `user_data == 1` matches the
`QUEUE_TOKEN` arm, the completion is consumed as a queue drain, and
**the registered handler is never called.**

The `IO_OP_TAG` doc comment currently says the scheme

> Requires readiness tokens passed to `register_readable` on the same port
> instance to stay below 2^63 -- true of every real caller (small
> sequential indices).

Small sequential indices are exactly the failure case if they start at 1.

### Reproduction

Standalone, no starlight involved — bind a `UnixListener`, register it,
run the proactor, connect twice:

| token | readiness events | clients accepted |
| --- | --- | --- |
| `1` | 0 | 0 |
| `1001` | 2 | 2 |

### Why it went unnoticed

Both existing `register_readable` callers independently picked large,
ASCII-derived tokens far above the reserved values:

- `camera_preview`: `CAMERA_STREAM_TOKEN = 0x4341_4d45_5241` (`"CAMERA"`)
- `network/src/p2p.rs`: `SNEAKERNET_READINESS_TOKEN = 0x4c4e_475f_4e45_5431`
  (`"LNG_NET1"`), plus a small per-fd index

So **no current caller is exposed** — checked 2026-09-08. The convention
that avoids the bug was already in use, just never written down or
enforced, and the first caller to reach for an obvious small token hit it
immediately.

### Suggested fix

`EpollPort` already solves this correctly:

```rust
const QUEUE_TAG: u64 = 1 << 32;
const WAKE_TAG:  u64 = 2 << 32;
```

Tagging the reserved values into a high range instead of squatting on `1`
and `2` makes small caller tokens safe. Porting that to `uring.rs` is the
minimal fix.

Failing that, `register_readable` should reject reserved tokens with
`ErrorKind::InvalidInput` rather than accepting a registration it will
never deliver. A silent no-op is the worst available behavior.

---

## 2. `IoPort::accept` leaks the accepted descriptor on an unrecognized peer family

**Severity:** descriptor leak, one per connection, until the process hits
its `RLIMIT_NOFILE`.

**Affects:** all four backends — `uring.rs`, `kqueue.rs`, `epoll.rs`, and
`iocp.rs`.

### What happens

Every backend resolves the peer address through `socket2`'s
`SockAddr::as_socket()`, which returns `Option<std::net::SocketAddr>`.
`std::net::SocketAddr` is IP-only, so `as_socket()` returns `None` for any
non-IP family — `AF_UNIX` most obviously.

The accept has already succeeded at that point. The kernel has created a
descriptor. The code then does:

```rust
match addr.to_socket_addr() {
    Some(peer) => Ok(AcceptTransfer { new_fd: result, peer }),
    None => Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "accept completed but the peer address family was unrecognized",
    )),
}
```

`result` — a live, open descriptor — is dropped on the `None` path
without being closed. The caller receives an error and has no way to
recover the descriptor to close it themselves.

Locations (all structurally identical):

| backend | file | notes |
| --- | --- | --- |
| io_uring | `proactor/src/uring.rs`, `InFlightOp::Accept` arm | `result` is the accepted fd |
| kqueue | `proactor/src/kqueue.rs`, `InFlightOp::Accept` arm | `new_fd` from `libc::accept` |
| epoll | `proactor/src/epoll.rs`, `InFlightOp::Accept` arm | `new_fd` from `libc::accept` |
| IOCP | `proactor/src/iocp.rs`, `finish_accept` | leaks `accept_socket`, already `SO_UPDATE_ACCEPT_CONTEXT`-associated; the `?` on `load_extension_fn` above leaks it too |

### Reproduction

Call `IoPort::accept` on a bound `AF_UNIX` listener and connect a client.
The handler receives `Err(InvalidData, "accept completed but the peer
address family was unrecognized")`, and `/proc/<pid>/fd` gains one socket
that is never released. Repeat until `EMFILE`.

### Why it went unnoticed

`proactor/tests/{uring,kqueue,epoll}.rs` all exercise `accept` over TCP
only, where `as_socket()` always succeeds. No in-tree caller accepts on a
Unix socket.

### Impact on starlight

This is why `starlight` does **not** use `IoPort::accept` for its thermal
signal socket. It registers the listener for readiness and calls a plain
`accept()` itself, which is reactor semantics rather than the true
proactor semantics the rest of that process uses. That compromise is
documented in `starlight/src/lib.rs::drain_pending_clients` and can be
removed once this is fixed.

### Suggested fix

Two parts, and the second is an API decision:

1. **Always close the descriptor on the error path**, in all four
   backends. This is a strict bug fix with no API change and should
   happen regardless of the below.
2. **Decide whether `AcceptTransfer` should represent non-IP peers.**

### Blast radius: three lines

Measured 2026-09-08, not estimated. `IoPort::accept` has **no consumers
outside this crate's own tests**. It is called in exactly three places —
`proactor/tests/{uring,kqueue,epoll}.rs` — and all three are the same TCP
test doing `accept_tx.send(transfer.peer)`.

Of the four crates depending on `loadngo-proactor`, none import
`AcceptTransfer`, `AcceptResult`, or `AcceptCompletionHandler`:

| crate | uses |
| --- | --- |
| `host-desktop` | `Proactor`, `ProactorHandle`, `CompletionKind`, `CompletionPort`, `RunReport`, `ReadinessEvent`, port types |
| `network` | `ChannelPort`, `Proactor`, `ProactorHandle`, `ReadinessPort`, `ReadinessEvent`, `CompletionKind` |
| `proactor-harness` | `CompletionKind`, `Proactor`, port types |
| `audio-io` | `ChannelPort`, `Proactor`, `ProactorHandle` |

So compatibility should not drive this decision. Changing the type costs
three test lines plus a channel type parameter today, and grows with every
future consumer. This is the cheapest it will ever be.

### Why `Option<SocketAddr>` is the weakest of the three

It looks attractive because `IoTransfer::peer` is already
`Option<SocketAddr>`, and that asymmetry is arguably what produced this
bug. But as the accept peer specifically, it carries three problems:

- **It creates a fail-open hazard that does not exist today.** The
  idiomatic consumption is `if let Some(peer) = transfer.peer { ...check
  peer... }`, which *skips the check entirely* when the peer is `None`.
  The compiler forces the caller to acknowledge the `None`, not to handle
  it safely, and the natural shape of the code fails open. A bare
  `SocketAddr` makes that mistake impossible.
- **`None` conflates two opposite situations**: "an `AF_UNIX` peer, which
  is normal" and "a sockaddr we could not parse, which is a fault". They
  warrant different responses, and collapsing them is exactly the
  ambiguity that hid this defect.
- **It is the wrong shape for a Unix peer.** Unnamed and abstract sockets
  have no path at all; the meaningful identity is `SO_PEERCRED`
  (uid/gid/pid). `None` encodes *absence* where the truth is *a different
  kind of identity*.

### Preferred: a small enum

```rust
pub enum PeerAddr {
    Ip(SocketAddr),
    Unix { path: Option<PathBuf> },   // None = unnamed or abstract
    Unknown { family: libc::sa_family_t },
}
```

Callers must `match`, so silently skipping a check is unnatural;
`Unknown` is visibly not an endorsement; and `SO_PEERCRED` can be added
later without a second breaking change.

### Cheaper middle ground

If the enum is more than wanted: keep *unparseable* as an error and make
only *known non-IP families* a `None`, by checking `ss_family` explicitly
rather than inferring from `as_socket()` returning `None`. `AF_UNIX` is a
legitimate peer; an `AF_INET` that failed to parse is a genuine fault.
That way `None` never means "something went wrong".

---

## 3. `IoUringPort::post` from another thread stalls until a blocked `poll` returns

Found and fixed 2026-10-05 (`43a58e2e`).

### What happens

`poll` holds the ring mutex for its whole blocking wait. `post` (behind
`enqueue_work`) woke the pump by submitting an `IORING_OP_NOP` tagged
`QUEUE_TOKEN`, which takes the same mutex. A post from any thread other than
the pump therefore waited until the pump's wait ended on its own: with a
deferred timer pending, until that timer fired; with none, until something
else woke it.

### Reproduction

Linux CI on `05413c28` (run 37314578861): `loadngo-inference`'s
`work_tools::run` reads a command's stdout and stderr on two threads and
posts the results. Every command waited out its full limit and reported
`timed_out`: `sh -c 'exit 3'` under a 30 s limit, `git status` held for 60 s.
`proactor/tests/uring.rs::uring_work_posted_from_another_thread_wakes_a_blocked_poll`
reproduces it directly: `run_once` waits on a 10 s timer, a second thread
posts, and the work must run within 2 s.

### Why it went unnoticed

Every existing uring test posted from the pump's own thread before polling,
and `wake()` already used the lock-free eventfd. Hosts that pump with
`run_ready` (the Linux desktop host) never hold the lock through a long wait.
Earlier CI runs of the code that exposed it had all been cancelled by newer
pushes.

### Fix

`post` writes the eventfd that `poll` already watches (`WAKE_TOKEN`), as
`wake()` does; the next `poll` drains the queue before it waits, so nothing
is lost. `signal_wake` was removed. The other three ports were already
lock-free here: `EpollPort` writes an eventfd, `KqueuePort` triggers an
`EVFILT_USER` event, `IocpPort` calls `PostQueuedCompletionStatus`.

### Verification

| where | result |
| --- | --- |
| Linux CI run 37316628556 on `43a58e2e` | fmt, clippy and the portable tests pass, including the new test and both `work_tools` tests that had failed |
| `dolores` (Pi 5, kernel 6.18.50+rpt-rpi-2712), `187fbc0b` | all 12 `tests/uring.rs` tests pass in 0.08 s; `work_tools` tests (with `--all-features`) pass in 0.32 s, where CI had waited 30 s and 60 s |
| `dolores`, pre-fix `uring.rs` (`43a58e2e^`) with the new test | the posted work runs after 9.99998 s, the full timer: the test fails, so it guards this defect |
| `agnes` (Pi 4, kernel 6.18.50+rpt-rpi-v8), `b3bdba83` | all 12 `tests/uring.rs` tests pass in 0.08 s; `work_tools` tests pass in 0.33 s; with the pre-fix `uring.rs` the new test fails at 9.99998 s |

## 4. `IocpPort` loses operations on a reused handle value

Found and fixed 2026-10-08. Jay chose registration with an unregistered
fallback after the profile below.

### What happens

An overlapped operation completes on an I/O completion port only if its
handle was associated with that port (`CreateIoCompletionPort`). `IocpPort`
associated each handle on its first operation and remembered the raw value
in a set, to skip the call afterwards. Windows gives a closed handle's value
to the next handle opened, usually on the very next open. The new handle's
value was already in the set, so it was never associated: its read's
completion went to no port, and `run_once` waited in
`GetQueuedCompletionStatus` with no timeout, forever.

espeak-ng-rs's `engine_io` (from its commit `84803914`) reads every engine
file by open, read, close on one process-wide proactor. Every Windows job of
its `Rust port` workflow from then on hung in three tests until GitHub's
six-hour limit: 17 jobs on 2026-10-08. Linux and macOS have no association
step and were unaffected. Any long-lived `Proactor<IocpPort>` that opens and
closes handles is exposed, including the Windows desktop host's.

### Reproduction

`tests/iocp.rs`: `reads_a_file_opened_after_another_was_closed` and
`sends_on_a_socket_created_after_another_was_closed` open, operate on and
close 64 files and UDP sockets on one port, and require a reused value.
With the raw-value cache, hosted Windows (CI run 37781126633, `4da86fae`)
failed both at cycle 1, on the first reused value (file `0x18c`, socket
`0x1d4`), in 10 s.

The proactor tests' waits now all go through `Proactor::run_once_until`
(`tests/support`), so a lost completion fails its test within 10 s instead
of hanging the job.

### Fix

`IoPort::register(fd)` (and `ProactorHandle::register`) associates a handle
with the port once and returns a tagged value, registration number << 32 |
handle; Windows guarantees handle values fit in 32 bits. Operations on a
tagged value look it up in the port's set and fail with `NotFound` once
`release` has removed it, rather than hang. An untagged handle, from a
caller that never registers, is associated before every operation, with
`ERROR_INVALID_PARAMETER` (already associated) taken as success. The port
never keys anything on a raw handle value. On the Unix backends `register`
returns `fd` unchanged and `release` does nothing, so callers need no
`cfg`. Registering is optional, worth about 0.8 us per operation on Windows.

### Strategies profiled

Before the choice, `IocpPort::with_association` picked one of three (since
removed):

| strategy | how | trade-off |
| --- | --- | --- |
| `RawValueCache` | the old cache | loses operations on reused values |
| `EveryOperation` | `CreateIoCompletionPort` before every operation; `ERROR_INVALID_PARAMETER` (87) means already associated | no API change; a failing system call per operation; cannot tell this port from another the handle is bound to |
| `Registered` | `IocpPort::register` associates once and returns registration number << 32 \| handle (Windows guarantees handle values fit in 32 bits); operations look the tag up; `release` forgets it | a set lookup per operation; every caller must register on open and release on close; a released or unknown tag fails with `NotFound`, never hangs; untagged values fall back to `EveryOperation` |

Proactor profile run 37782289447, `f7747dda`, `windows-latest`, 7 rounds,
median ns per operation, each operation waited for before the next:

| scenario | RawValueCache | EveryOperation | Registered |
| --- | ---: | ---: | ---: |
| association step alone | 16 | 825 (87 on all 200,000 calls) | 16 |
| 4 KiB file read, one open file | 3,388 | 4,189 (+24%) | 3,382 |
| UDP send_to + recv_from | 8,505 | 10,060 (+18%) | 8,394 |
| open, read 4 KiB, close | stalled at cycle 1 | 26,962 | 27,112 |
| bind, send_to, close | stalled at cycle 1 | 125,386 | 120,700 |

`Registered` costs what the broken cache did and works under churn;
`EveryOperation` adds about 0.8 µs, a system call, to every operation.

### Other findings from the general profile (same run)

Not defects in this sense, but worth knowing; medians:

- IOCP timers are coarse: a 1 ms `defer_for` fires 12.7 ms late and an
  idle 100 ms wait returns 10.6 ms late, the default 15.6 ms Windows timer
  tick. Fixed the same day: defect 5. (This note first said frame pacing
  through `FrameDemand::After` inherits it; it does not, see defect 5.)
- kqueue (macOS runner): 1 ms timer 163 µs late (p99 2.1 ms), idle wait
  1.06 ms late, likely kernel timer coalescing.
- epoll file reads go through worker threads: 21–25 µs per 4 KiB read
  against 1.7–2.3 µs on io_uring. io_uring is available on GitHub's Linux
  runners, so CI's Linux tests exercise it.
- Cross-thread wake (post to a pump blocked in `run_once`) is 16–19 µs on
  the Linux runners, 6.8 µs on macOS, 0.7 µs on Windows.

## 5. `IocpPort` ends deadlines on Windows' 15.6 ms timer tick

Found and fixed 2026-10-08, from the general profile above. Jay chose the
fix after two more profiles of the candidates.

### What happens

A proactor turn waits for completions until its next deferred deadline.
`IocpPort::poll` waited in `GetQueuedCompletionStatus` with that deadline
as a millisecond timeout, and Windows ends such a wait on its timer tick,
15.625 ms unless a process has raised the resolution. A deadline therefore
fired anywhere up to a tick late. On GitHub's `windows-latest` runner a
1 ms `defer_for` fired 12.7–12.9 ms late (median; p99 15.1–15.3 ms) and an idle
100 ms wait returned 1.9–10.6 ms late (p99 14.5 ms) across three runs.

### Who it reached, and who it did not

- Reached: every proactor driven by `run_once`/`run_until_stopped` on an
  `IocpPort`, such as espeak-ng-rs's engine reads and its playback events
  delivered from proactor timers, kimi's sampling and tool deadlines.
- Not reached: the Windows desktop host's frames. `host-desktop`'s
  `windows.rs` drains its proactor without blocking and hands the next
  deadline to winit (`ControlFlow::WaitUntil`), and winit 0.30 waits on a
  high-resolution waitable timer (`CREATE_WAITABLE_TIMER_HIGH_RESOLUTION`)
  in `MsgWaitForMultipleObjectsEx`. `std::thread::sleep` uses the same
  kind of timer. So `FrameDemand::After` pacing was never affected.

### Candidates profiled

`IocpPort::with_timer_wait` (on branch `iocp-timers` only, since removed)
picked one of:

| candidate | how | trade-off |
| --- | --- | --- |
| millisecond (the old wait) | `GetQueuedCompletionStatus` timeout | ends on the 15.6 ms tick |
| `timeBeginPeriod(1)` | the same wait with the process's timer resolution raised to 1 ms for the port's life | no per-wait cost; more timer interrupts the whole time a port exists, even idle, which the thermal rules argue against; Windows 11 may not honor it for a minimized window |
| wait packet | a high-resolution waitable timer set to the deadline, associated with the port by `NtAssociateWaitCompletionPacket`; the wait has no timeout and the timer's packet ends it | no change to the system tick, no timer while nothing is due; uses ntdll functions Microsoft does not document but the Windows thread pool and Go's runtime rely on |

A waitable timer with an APC and an alertable `GetQueuedCompletionStatusEx`
was not tried: that call reports failed I/O as NTSTATUS codes in its batch
entries, so the port's error reporting would have changed with it.

Figures: `proactor-profile` on `windows-latest`, every operation waited for
before the next, nanoseconds, median (p99). The clock is
QueryPerformanceCounter at 10 MHz (100 ns tick). Each run is a different
runner, so compare within a column, not across runs.

First run, 37790218965 (`64d7ad3a`): the wait packet armed the timer and
withdrew it again on every timed poll.

| | millisecond | `timeBeginPeriod(1)` | wait packet |
| --- | ---: | ---: | ---: |
| 1 ms deadline, how late | 12,733,800 (15,348,000) | 636,900 (2,000,400) | 535,500 (552,000) |
| idle 100 ms wait, how late | 1,927,000 (13,721,300) | 490,700 (2,000,600) | 387,800 (657,600) |
| 4 KiB file read | 2,900 | 2,800 | 3,500 |
| 64 KiB file read | 6,000 | 5,800 | 12,100 |
| UDP send_to + recv_from | 5,500 | 5,400 | 7,100 |

The wait packet's extra I/O cost was four system calls per poll (set and
associate, cancel and withdraw), paid on every operation because the
profile waits with a 2 s deadline pending. The fix keeps the timer set
across polls that other work ends while the deadline holds.

Second run, 37791909803 (`411a0a22`), timer kept armed:

| | millisecond | `timeBeginPeriod(1)` | wait packet |
| --- | ---: | ---: | ---: |
| 1 ms deadline, how late | 12,878,000 (15,288,100) | 1,037,800 (2,020,300) | 540,000 (589,000) |
| idle 100 ms wait, how late | 4,650,700 (14,459,900) | 495,200 (2,044,700) | 232,300 (708,000) |
| 4 KiB file read | 4,700 | 4,800 | 4,700 |
| 64 KiB file read | 8,700 | 8,700 | 8,800 |
| UDP send_to + recv_from | 9,100 | 9,200 | 9,200 |
| enqueue / cross-thread wake (batches of 1000) | 957 / 889 | 762 / 773 | 756 / 784 |

### Fix

`84b2b6a8`: the wait packet is how every `IocpPort` waits out a deadline;
the switch and the other candidates are gone.

- A poll with a deadline sets the port's high-resolution timer for it (in
  100 ns units, relative), associates the timer with the port under its
  own completion key, and waits with no timeout. The timer's packet ends
  the wait as a timeout.
- The timer stays set across polls that I/O or posted work ends while the
  deadline holds (within 100 us, since each poll recomputes it from a
  slightly later "now"); it is withdrawn when the deadline moves or no
  deadline remains, and re-armed after it fires.
- One poll owns the timer at a time. A poll that does not own it (a second
  polling thread, or a non-blocking `run_ready`) and dequeues its packet
  flags the timer as fired for the owner and, if the owner is waiting on
  it at that moment, wakes it. A packet already being queued when it is
  withdrawn reaches a later poll as an early, harmless timeout.
- Where the timer or packet cannot be created (before Windows 10 1803), and
  for a concurrent poller, polls wait in milliseconds as before.

### Verification

| where | result |
| --- | --- |
| `windows-latest`, run 37791909803 (`411a0a22`, same code behind the switch) | 20 of 20 IOCP tests, including: a 1 ms deadline within 4 ms (median of 21); deadlines on time after 10 waits ended by work posted from another thread; and after a non-blocking `run_ready` took the timer's packet with nothing waiting |
| `windows-latest`, `84b2b6a8` (the fix as landed) | CI run 37793143051 green on all four platforms; profile run 37793143057: 19 of 19 IOCP tests, 1 ms deadline 540,600 ns late (p99 570,300), idle 100 ms wait 248,500 (p99 747,900), 4 KiB read 4,800, 64 KiB read 8,700, UDP pair 9,300, enqueue 767, cross-thread 757 |
| macOS, Linux | unaffected: `iocp.rs` compiles only for Windows; clippy `-D warnings` cross-checked for both |

## Related

- `docs/PROACTOR_ENGINE_ADOPTION.md` — adoption status and the host contract
- `docs/PROACTOR_ARCHITECTURE.md` — the `HostProactor` seam
- `starlight/src/runtime.rs` — the migration that surfaced both defects
