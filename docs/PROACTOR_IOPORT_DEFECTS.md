# Proactor IoPort Defects

Status: **open**, both found 2026-09-08 while migrating `starlight` onto
`Proactor<IoUringPort>` (see that repo's `src/runtime.rs`). Neither is
fixed. Both are recorded here rather than fixed in place because
`loadngo-proactor` is shared by `host-desktop` and three games, and the
second one has a public-API dimension worth deciding deliberately.

Neither defect is hypothetical: the first one silently broke starlight's
thermal socket on real hardware, and the second is the reason starlight
does not use `IoPort::accept` at all.

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

`camera_preview` — the only in-tree `register_readable` caller — uses
`CAMERA_STREAM_TOKEN = 0x4341_4d45_5241` (`"CAMERA"`), which is far above
the reserved values. The convention that avoids the bug was already in
use, just never written down or enforced.

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
   Options, roughly in increasing order of disruption:
   - keep `peer: SocketAddr` and return the fd anyway with a sentinel —
     poor, loses information;
   - change to `peer: Option<SocketAddr>`, mirroring
     `IoTransfer::peer`, which is already `Option<SocketAddr>` for
     exactly this kind of "may not be known" case. Breaking, but small,
     and consistent with a type already in the same module;
   - add a separate address enum covering `AF_UNIX`. Most correct, most
     work.

`IoTransfer::peer` being `Option<SocketAddr>` already while
`AcceptTransfer::peer` is a bare `SocketAddr` is itself the asymmetry
that produced this bug.

---

## Related

- `docs/PROACTOR_ENGINE_ADOPTION.md` — adoption status and the host contract
- `docs/PROACTOR_ARCHITECTURE.md` — the `HostProactor` seam
- `starlight/src/runtime.rs` — the migration that surfaced both defects
