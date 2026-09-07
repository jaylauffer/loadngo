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

## Related

- `docs/PROACTOR_ENGINE_ADOPTION.md` — adoption status and the host contract
- `docs/PROACTOR_ARCHITECTURE.md` — the `HostProactor` seam
- `starlight/src/runtime.rs` — the migration that surfaced both defects
