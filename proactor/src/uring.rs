use crate::{
    AcceptCompletionHandler, AcceptResult, AcceptTransfer, CompletionEnvelope, CompletionPort,
    IoBuf, IoCompletionHandler, IoOpId, IoPort, IoResult, IoTransfer, PollEvent, ReadinessEvent,
    ReadinessPort, UnitCompletionHandler,
};
use io_uring::{opcode, squeue, types, IoUring};
use libc::{timespec, POLLERR, POLLHUP, POLLIN, POLLRDHUP};
use socket2::SockAddr;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

const QUEUE_TOKEN: u64 = 1;
const WAKE_TOKEN: u64 = 2;
const MAX_EVENTS: usize = 256;
const READINESS_POLL_MASK: u32 = (POLLIN | POLLERR | POLLHUP | POLLRDHUP) as u32;
/// Every `IoOpId` this backend allocates has this bit set, letting
/// `poll()`'s CQE dispatch distinguish an `IoPort` operation's completion
/// from `QUEUE_TOKEN`/`WAKE_TOKEN`/a `register_readable` token without a
/// separate side table lookup just to find out which kind of `user_data`
/// it's looking at. Requires readiness tokens passed to
/// `register_readable` on the same port instance to stay below 2^63 --
/// true of every real caller (small sequential indices).
const IO_OP_TAG: u64 = 1 << 63;

/// A ring-mutating operation requested by `register_readable`/`deregister`
/// or an `IoPort` method, deferred until the next time `poll()` actually
/// holds the `ring` lock (see the module-level note on `pending_ops` for
/// why this can't just lock `ring` directly).
enum PendingRingOp {
    Register {
        fd: RawFd,
        token: u64,
    },
    Deregister {
        token: u64,
    },
    /// A fully-built SQE from an `IoPort` method that lost the `try_lock`
    /// race -- pushed as-is once `poll()` next holds the lock, same
    /// deferral shape as `Register`/`Deregister`.
    Submit(squeue::Entry),
}

/// What `read`/`write`/`recv`/`send`/`recv_from`/`send_to`/`accept`/
/// `connect` need kept alive (the buffer, the handler, and -- for the
/// address-carrying ops -- the kernel-facing sockaddr storage) from
/// submission until the CQE naming this op's `IoOpId` arrives. Boxed as a
/// whole so its heap address (and therefore every raw pointer an SQE
/// holds into it) never moves even if the surrounding `HashMap` rehashes.
enum InFlightOp {
    Read {
        buf: IoBuf,
        handler: Box<dyn IoCompletionHandler>,
    },
    Write {
        buf: IoBuf,
        handler: Box<dyn IoCompletionHandler>,
    },
    Recv {
        buf: IoBuf,
        handler: Box<dyn IoCompletionHandler>,
    },
    Send {
        buf: IoBuf,
        handler: Box<dyn IoCompletionHandler>,
    },
    RecvFrom {
        buf: IoBuf,
        handler: Box<dyn IoCompletionHandler>,
        state: Box<RecvFromState>,
    },
    SendTo {
        buf: IoBuf,
        handler: Box<dyn IoCompletionHandler>,
        _state: Box<SendToState>,
    },
    Accept {
        handler: Box<dyn AcceptCompletionHandler>,
        addr: Box<RawSockAddr>,
    },
    Connect {
        handler: Box<dyn UnitCompletionHandler>,
        // Only needs to stay alive for the kernel to read from; never
        // written back into, unlike Accept/RecvFrom's addr.
        _addr: Box<SockAddr>,
    },
}

pub struct IoUringPort {
    ring: Mutex<IoUring>,
    queue: Mutex<VecDeque<CompletionEnvelope>>,
    wake_fd: RawFd, // eventfd for waking
    registered: Mutex<HashMap<RawFd, u64>>,
    // `poll()` holds `ring`'s lock for the *entire* duration of its
    // blocking `submit_and_wait`/`submit_with_args` call below, which can
    // legitimately take an unbounded amount of time (that's the whole
    // point of a blocking wait). `register_readable`/`deregister` used to
    // lock `ring` directly to submit their own SQE — which deadlocks
    // outright if called from another thread while `poll()` is mid-wait,
    // since that lock is held for exactly the duration nothing else can
    // ever acquire it. Confirmed as a real (not just theoretical) hang via
    // `network::registered_proactor_pump_handles_dual_stack_node_sockets`.
    // Fix: queue the request here instead of touching `ring`, then
    // interrupt any in-progress wait via `wake()` — which only writes to
    // `wake_fd` via a raw syscall and never touches `ring` at all, so it
    // can never deadlock against a blocked `poll()` the way locking `ring`
    // directly could. `poll()` drains and submits this queue itself, at
    // the top of its own iteration, where it already safely holds the
    // lock (not blocked in the kernel). This mirrors the exact same
    // queue-then-wake shape `post()`/`drain_completion()` already use for
    // completions — just extended to cover readiness registration too.
    pending_ops: Mutex<VecDeque<PendingRingOp>>,
    /// Every `IoPort` operation still waiting on its completion CQE, keyed
    /// by the `IoOpId` its SQE's `user_data` carries. An entry is only
    /// ever removed by `poll()` when that CQE actually arrives (naturally
    /// or via cancellation) -- never by `cancel_io`, which merely
    /// requests cancellation and leaves the entry (and its buffer) in
    /// place until the kernel actually confirms the op is done.
    in_flight: Mutex<HashMap<IoOpId, InFlightOp>>,
    next_io_op_id: AtomicU64,
}

impl IoUringPort {
    pub fn new() -> io::Result<Self> {
        let ring = IoUring::new(MAX_EVENTS as u32).map_err(|e| io::Error::other(e.to_string()))?;

        let wake_fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if wake_fd == -1 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            ring: Mutex::new(ring),
            queue: Mutex::new(VecDeque::new()),
            wake_fd,
            registered: Mutex::new(HashMap::new()),
            pending_ops: Mutex::new(VecDeque::new()),
            in_flight: Mutex::new(HashMap::new()),
            next_io_op_id: AtomicU64::new(0),
        })
    }

    fn allocate_io_op_id(&self) -> IoOpId {
        IoOpId(self.next_io_op_id.fetch_add(1, Ordering::Relaxed) | IO_OP_TAG)
    }

    /// Submits `sqe` now if `ring` is uncontended, or defers it via
    /// `pending_ops` (same try-lock-or-queue-and-wake shape
    /// `register_readable` already uses, see that method and the
    /// `pending_ops` field doc) if `poll()` currently holds the lock
    /// mid-wait. Every `IoPort` method funnels through this rather than
    /// locking `ring` directly, for exactly the reason documented there.
    fn submit_or_defer(&self, sqe: squeue::Entry) -> io::Result<()> {
        match self.ring.try_lock() {
            Ok(mut ring) => {
                let result: io::Result<()> = (|| {
                    unsafe {
                        ring.submission()
                            .push(&sqe)
                            .map_err(|_| io::Error::other("submission queue full"))?;
                    }
                    ring.submit().map_err(|e| io::Error::other(e.to_string()))?;
                    Ok(())
                })();
                result
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                self.pending_ops
                    .lock()
                    .expect("io_uring pending-ops queue poisoned")
                    .push_back(PendingRingOp::Submit(sqe));
                self.wake()
            }
            Err(std::sync::TryLockError::Poisoned(err)) => {
                Err(io::Error::other(format!("lock poisoned: {err}")))
            }
        }
    }

    /// Submits every queued `register_readable`/`deregister` request onto
    /// `ring`. Only ever called from `poll()`, which already holds
    /// `ring`'s lock at the time — never call this while also holding a
    /// separate lock on `ring`, and never call it instead of holding one.
    /// A push failure here (submission queue full) is dropped rather than
    /// surfaced: at `MAX_EVENTS` = 256 deep it should be exceedingly rare,
    /// and silently dropping one readiness registration is far better than
    /// the deadlock this queue exists to avoid.
    fn apply_pending_ops(&self, ring: &mut IoUring) {
        let ops: Vec<PendingRingOp> = self
            .pending_ops
            .lock()
            .expect("io_uring pending-ops queue poisoned")
            .drain(..)
            .collect();

        for op in ops {
            let sqe = match op {
                PendingRingOp::Register { fd, token } => {
                    opcode::PollAdd::new(types::Fd(fd), READINESS_POLL_MASK)
                        .multi(true)
                        .build()
                        .user_data(token)
                }
                PendingRingOp::Deregister { token } => {
                    opcode::PollRemove::new(token).build().user_data(WAKE_TOKEN)
                }
                PendingRingOp::Submit(sqe) => sqe,
            };
            unsafe {
                let _ = ring.submission().push(&sqe);
            }
        }
    }

    fn drain_completion(&self) -> Option<CompletionEnvelope> {
        self.queue
            .lock()
            .expect("io_uring completion queue poisoned")
            .pop_front()
    }

    /// Looks up and removes `op_id`'s `InFlightOp` (its buffer/handler/
    /// address storage can finally be dropped -- the kernel is done with
    /// them, this CQE is the proof) and builds a ready-to-run thunk from
    /// `result` (io_uring's CQE `res` field: negative `-errno` on
    /// failure, otherwise bytes transferred or, for `Accept`, the new
    /// fd). Returns a benign `PollEvent::Wake` for a `op_id` with no
    /// matching entry -- shouldn't happen in practice, but a stray/
    /// duplicate CQE is far better tolerated than panicking on it.
    fn resolve_io_completion(&self, op_id: IoOpId, result: i32) -> PollEvent {
        let entry = self
            .in_flight
            .lock()
            .expect("io_uring in-flight op table poisoned")
            .remove(&op_id);

        let Some(entry) = entry else {
            return PollEvent::Wake;
        };

        let ok = result >= 0;
        let err = || io::Error::from_raw_os_error(-result);

        let thunk: Box<dyn FnOnce() + Send> = match entry {
            InFlightOp::Read { mut buf, handler } | InFlightOp::Recv { mut buf, handler } => {
                let io_result: IoResult = if ok {
                    unsafe { buf.set_filled_len(result as usize) };
                    Ok(IoTransfer {
                        buf,
                        bytes_transferred: result as u32,
                        peer: None,
                    })
                } else {
                    Err(err())
                };
                Box::new(move || handler.run(io_result))
            }
            InFlightOp::Write { buf, handler } | InFlightOp::Send { buf, handler } => {
                let io_result: IoResult = if ok {
                    Ok(IoTransfer {
                        buf,
                        bytes_transferred: result as u32,
                        peer: None,
                    })
                } else {
                    Err(err())
                };
                Box::new(move || handler.run(io_result))
            }
            InFlightOp::RecvFrom {
                mut buf,
                handler,
                state,
            } => {
                let io_result: IoResult = if ok {
                    unsafe { buf.set_filled_len(result as usize) };
                    Ok(IoTransfer {
                        buf,
                        bytes_transferred: result as u32,
                        peer: state.addr.to_socket_addr(),
                    })
                } else {
                    Err(err())
                };
                Box::new(move || handler.run(io_result))
            }
            InFlightOp::SendTo {
                buf,
                handler,
                _state: _,
            } => {
                let io_result: IoResult = if ok {
                    Ok(IoTransfer {
                        buf,
                        bytes_transferred: result as u32,
                        peer: None,
                    })
                } else {
                    Err(err())
                };
                Box::new(move || handler.run(io_result))
            }
            InFlightOp::Accept { handler, addr } => {
                let accept_result: AcceptResult = if ok {
                    match addr.to_socket_addr() {
                        Some(peer) => Ok(AcceptTransfer {
                            new_fd: result,
                            peer,
                        }),
                        None => Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "accept completed but the peer address family was unrecognized",
                        )),
                    }
                } else {
                    Err(err())
                };
                Box::new(move || handler.run(accept_result))
            }
            InFlightOp::Connect { handler, .. } => {
                let unit_result = if ok { Ok(()) } else { Err(err()) };
                Box::new(move || handler.run(unit_result))
            }
        };

        PollEvent::IoCompletion(thunk)
    }

    fn duration_to_timespec(duration: Duration) -> timespec {
        timespec {
            tv_sec: duration.as_secs() as _,
            tv_nsec: duration.subsec_nanos() as _,
        }
    }

    fn signal_wake(&self, token: u64) -> io::Result<()> {
        let mut ring = self
            .ring
            .lock()
            .map_err(|e| io::Error::other(format!("lock poisoned: {}", e)))?;

        let noop = opcode::Nop::new().build().user_data(token);

        unsafe {
            ring.submission()
                .push(&noop)
                .map_err(|_| io::Error::other("submission queue full"))?;
        }

        ring.submit().map_err(|e| io::Error::other(e.to_string()))?;

        Ok(())
    }

    fn signal_eventfd(fd: RawFd) -> io::Result<()> {
        let value: u64 = 1;
        let rc = unsafe {
            libc::write(
                fd,
                &value as *const u64 as *const libc::c_void,
                std::mem::size_of::<u64>(),
            )
        };
        if rc == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn clear_eventfd(fd: RawFd) -> io::Result<()> {
        loop {
            let mut value = 0u64;
            let rc = unsafe {
                libc::read(
                    fd,
                    &mut value as *mut u64 as *mut libc::c_void,
                    std::mem::size_of::<u64>(),
                )
            };
            if rc == -1 {
                let err = io::Error::last_os_error();
                match err.raw_os_error() {
                    Some(libc::EAGAIN) => return Ok(()),
                    Some(libc::EINTR) => continue,
                    _ => return Err(err),
                }
            }
            if rc == 0 {
                return Err(io::Error::from_raw_os_error(libc::EINVAL));
            }
        }
    }
}

impl CompletionPort for IoUringPort {
    fn post(&self, envelope: CompletionEnvelope) -> io::Result<()> {
        self.queue
            .lock()
            .expect("io_uring completion queue poisoned")
            .push_back(envelope);

        // Signal via IORING_OP_NOP with QUEUE_TOKEN
        self.signal_wake(QUEUE_TOKEN)
    }

    fn poll(&self, timeout: Option<Duration>) -> io::Result<PollEvent> {
        if let Some(envelope) = self.drain_completion() {
            return Ok(PollEvent::Completion(envelope));
        }

        let mut ring = self
            .ring
            .lock()
            .map_err(|e| io::Error::other(format!("lock poisoned: {}", e)))?;

        // Submit anything register_readable/deregister queued while we
        // might have been blocked (or simply not running) — safe here,
        // we hold the lock and aren't inside the blocking wait below yet.
        self.apply_pending_ops(&mut ring);

        // Register eventfd for poll if not already registered
        // We need to submit a POLL_ADD for wake_fd
        let poll_op = opcode::PollAdd::new(types::Fd(self.wake_fd), libc::POLLIN as u32)
            .build()
            .user_data(WAKE_TOKEN);

        unsafe {
            ring.submission()
                .push(&poll_op)
                .map_err(|_| io::Error::other("submission queue full"))?;
        }

        ring.submit().map_err(|e| io::Error::other(e.to_string()))?;

        // Wait for events. Retries on EINTR: a stray signal delivered to
        // this thread during the blocking io_uring_enter (ptrace attach,
        // SIGCHLD from an unrelated part of the process, etc.) must not be
        // allowed to propagate as an error here -- found via
        // proactor-harness's throughput bench, where an EINTR from an
        // strace attach killed the whole pump thread (run_until_stopped
        // propagates poll()'s Err via `?`), silently stopping all
        // dispatch for the rest of the process's life. Any submitted SQEs
        // (the wake_fd poll above) are already in the kernel by this
        // point regardless of EINTR, so retrying just the wait -- not
        // re-pushing anything -- is correct. This does mean an
        // interrupted wait effectively restarts its timeout rather than
        // using the remaining duration; acceptable since EINTR here is
        // rare and the reactor's own deferred-queue deadline is
        // recomputed fresh on the next full poll() call anyway.
        let result = loop {
            let attempt = if let Some(duration) = timeout {
                let ts = Self::duration_to_timespec(duration);
                let uring_ts = types::Timespec::new()
                    .sec(ts.tv_sec as _)
                    .nsec(ts.tv_nsec as _);
                let args = types::SubmitArgs::new().timespec(&uring_ts);
                ring.submitter().submit_with_args(1, &args)
            } else {
                ring.submit_and_wait(1)
            };
            match attempt {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                other => break other,
            }
        };

        match result {
            Ok(_) => {}
            // `submit_with_args`'s own wait-timeout (the `Some(duration)`
            // branch above, via IORING_ENTER_EXT_ARG) signals via errno
            // ETIME, not ETIMEDOUT -- a real, pre-existing gap, not
            // something this change introduces: Rust's std only maps
            // ETIMEDOUT to ErrorKind::TimedOut, so ETIME falls through to
            // ErrorKind::Uncategorized and used to propagate as a raw
            // error here. Never caught before because every prior
            // run_ready()-based test happened to already have its
            // completion sitting in the CQ on the very first zero-duration
            // poll, never genuinely exercising this timeout path -- found
            // via a new IoPort test whose real (if short) disk I/O latency
            // finally forced a genuine wait.
            Err(e)
                if e.kind() == io::ErrorKind::TimedOut || e.raw_os_error() == Some(libc::ETIME) =>
            {
                return Ok(PollEvent::Timeout);
            }
            Err(e) => return Err(e),
        }

        // Process completion queue
        let mut cq = ring.completion();
        if let Some(cqe) = cq.next() {
            match cqe.user_data() {
                QUEUE_TOKEN => {
                    if let Some(envelope) = self.drain_completion() {
                        Ok(PollEvent::Completion(envelope))
                    } else {
                        Ok(PollEvent::Wake)
                    }
                }
                WAKE_TOKEN => {
                    Self::clear_eventfd(self.wake_fd)?;
                    Ok(PollEvent::Wake)
                }
                token if token & IO_OP_TAG != 0 => {
                    Ok(self.resolve_io_completion(IoOpId(token), cqe.result()))
                }
                token => {
                    let is_registered = self
                        .registered
                        .lock()
                        .expect("io_uring readiness registry poisoned")
                        .values()
                        .any(|registered_token| *registered_token == token);
                    if is_registered {
                        Ok(PollEvent::Readiness(ReadinessEvent { token }))
                    } else {
                        // Stray completion (e.g. the ack for a cancelled poll). Treat
                        // it as a benign wake rather than an unknown readiness token.
                        Ok(PollEvent::Wake)
                    }
                }
            }
        } else {
            Ok(PollEvent::Timeout)
        }
    }

    fn wake(&self) -> io::Result<()> {
        Self::signal_eventfd(self.wake_fd)
    }

    fn begin_shutdown(&self) {
        let op_ids: Vec<IoOpId> = self
            .in_flight
            .lock()
            .expect("io_uring in-flight op table poisoned")
            .keys()
            .copied()
            .collect();
        for op_id in op_ids {
            // Best-effort: a submission failure here just means this op's
            // own natural completion (success, or whatever error it hits
            // on its own) is what eventually clears it from `in_flight`
            // instead of an early cancellation -- still correct, just not
            // expedited.
            let _ = self.cancel_io(op_id);
        }
    }

    fn shutdown_complete(&self) -> bool {
        self.in_flight
            .lock()
            .expect("io_uring in-flight op table poisoned")
            .is_empty()
    }
}

impl ReadinessPort for IoUringPort {
    fn register_readable(&self, fd: RawFd, token: u64) -> io::Result<()> {
        {
            let mut registered = self
                .registered
                .lock()
                .expect("io_uring readiness registry poisoned");
            if registered.contains_key(&fd) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("fd {fd} already registered"),
                ));
            }
            registered.insert(fd, token);
        }

        let poll_op = opcode::PollAdd::new(types::Fd(fd), READINESS_POLL_MASK)
            .multi(true)
            .build()
            .user_data(token);

        // `try_lock` rather than `lock`: when it succeeds, nothing else
        // holds `ring` right now, so it's safe (and matches the original,
        // fully synchronous behavior every existing caller already relies
        // on) to submit directly. When it fails, `ring` is held by a
        // concurrent `poll()` call — almost certainly blocked inside its
        // wait, since that's the only place this lock is held for any
        // real length of time — and locking here would deadlock exactly
        // as it used to (see `pending_ops`'s doc comment). Queue for that
        // `poll()` to pick up instead, and `wake()` it so it doesn't sit
        // on its current wait indefinitely.
        match self.ring.try_lock() {
            Ok(mut ring) => {
                let submit_result: io::Result<()> = (|| {
                    unsafe {
                        ring.submission()
                            .push(&poll_op)
                            .map_err(|_| io::Error::other("submission queue full"))?;
                    }
                    ring.submit().map_err(|e| io::Error::other(e.to_string()))?;
                    Ok(())
                })();
                drop(ring);

                if let Err(err) = submit_result {
                    self.registered
                        .lock()
                        .expect("io_uring readiness registry poisoned")
                        .remove(&fd);
                    return Err(err);
                }

                Ok(())
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                self.pending_ops
                    .lock()
                    .expect("io_uring pending-ops queue poisoned")
                    .push_back(PendingRingOp::Register { fd, token });
                self.wake()
            }
            Err(std::sync::TryLockError::Poisoned(err)) => {
                Err(io::Error::other(format!("lock poisoned: {err}")))
            }
        }
    }

    fn deregister(&self, fd: RawFd) -> io::Result<()> {
        let token = self
            .registered
            .lock()
            .expect("io_uring readiness registry poisoned")
            .remove(&fd);

        let Some(token) = token else {
            return Ok(());
        };

        let remove_op = opcode::PollRemove::new(token).build().user_data(WAKE_TOKEN);

        // Same try_lock/queue split as register_readable above.
        match self.ring.try_lock() {
            Ok(mut ring) => {
                unsafe {
                    ring.submission()
                        .push(&remove_op)
                        .map_err(|_| io::Error::other("submission queue full"))?;
                }
                ring.submit().map_err(|e| io::Error::other(e.to_string()))?;
                Ok(())
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                self.pending_ops
                    .lock()
                    .expect("io_uring pending-ops queue poisoned")
                    .push_back(PendingRingOp::Deregister { token });
                self.wake()
            }
            Err(std::sync::TryLockError::Poisoned(err)) => {
                Err(io::Error::other(format!("lock poisoned: {err}")))
            }
        }
    }
}

/// Raw kernel-facing address storage for the ops where the *kernel*
/// fills it in asynchronously (`accept`, `recv_from`'s `msghdr`).
/// `socket2::SockAddr` doesn't fit that case -- its own API
/// (`try_init`/`from`) is built around a synchronous "call a syscall now,
/// validate what it wrote" pattern, not "hand over raw storage, the
/// kernel writes into it whenever the CQE eventually arrives". Converted
/// to `socket2::SockAddr` (for its safe, well-tested parsing into
/// `std::net::SocketAddr`) only once a completion actually reports it.
struct RawSockAddr {
    storage: libc::sockaddr_storage,
    len: libc::socklen_t,
}

impl RawSockAddr {
    fn empty() -> Self {
        Self {
            storage: unsafe { std::mem::zeroed() },
            len: std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t,
        }
    }

    fn as_mut_sockaddr_ptr(&mut self) -> *mut libc::sockaddr {
        std::ptr::addr_of_mut!(self.storage).cast()
    }

    fn len_mut_ptr(&mut self) -> *mut libc::socklen_t {
        std::ptr::addr_of_mut!(self.len)
    }

    fn to_socket_addr(&self) -> Option<SocketAddr> {
        unsafe { SockAddr::new(self.storage, self.len) }.as_socket()
    }
}

/// Everything `recv_from` needs kept alive at a stable address for the
/// whole operation: the `msghdr` the SQE points to, the single `iovec`
/// that `msghdr` itself points to (covering the whole buffer), and the
/// raw address storage `msghdr` points to as `msg_name`. All three live
/// in one struct (always behind a `Box`) so moving the `Box` handle
/// itself -- e.g. a `HashMap` rehash -- never invalidates the pointers
/// each field holds into the others.
struct RecvFromState {
    iov: libc::iovec,
    msg: libc::msghdr,
    addr: RawSockAddr,
}

// SAFETY: `iovec`/`msghdr`'s raw pointers here only ever point at other
// fields of this same struct (self-referential, stable behind the `Box`
// that always wraps it) or into an `IoBuf`'s own heap allocation that
// this same `InFlightOp` co-owns for the operation's whole lifetime --
// nothing thread-affine, no aliasing across threads without already
// holding `IoUringPort::in_flight`'s lock. Needed because raw pointers
// are `!Send` by default, and this type only ever lives inside a
// `Mutex`-guarded table (which requires `T: Send`, not `Sync`).
unsafe impl Send for RecvFromState {}

/// The `send_to` analogue of `RecvFromState`: everything needed kept
/// alive at a stable address for the whole operation. `addr` is already
/// fully known up front here (the caller's `target`), unlike
/// `RecvFromState`'s kernel-filled `RawSockAddr` -- `socket2::SockAddr`
/// fits this direction fine.
struct SendToState {
    iov: libc::iovec,
    msg: libc::msghdr,
    addr: SockAddr,
}

// SAFETY: same reasoning as `RecvFromState` above.
unsafe impl Send for SendToState {}

impl IoPort for IoUringPort {
    fn read(
        &self,
        fd: RawFd,
        mut buf: IoBuf,
        offset: u64,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        let op_id = self.allocate_io_op_id();
        let sqe = opcode::Read::new(types::Fd(fd), buf.as_mut_ptr(), buf.capacity() as u32)
            .offset(offset)
            .build()
            .user_data(op_id.0);
        self.in_flight
            .lock()
            .expect("io_uring in-flight op table poisoned")
            .insert(
                op_id,
                InFlightOp::Read {
                    buf,
                    handler: Box::new(handler),
                },
            );
        self.submit_or_defer(sqe)?;
        Ok(op_id)
    }

    fn write(
        &self,
        fd: RawFd,
        buf: IoBuf,
        offset: u64,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        let op_id = self.allocate_io_op_id();
        let sqe = opcode::Write::new(types::Fd(fd), buf.as_ptr(), buf.len() as u32)
            .offset(offset)
            .build()
            .user_data(op_id.0);
        self.in_flight
            .lock()
            .expect("io_uring in-flight op table poisoned")
            .insert(
                op_id,
                InFlightOp::Write {
                    buf,
                    handler: Box::new(handler),
                },
            );
        self.submit_or_defer(sqe)?;
        Ok(op_id)
    }

    fn recv(
        &self,
        fd: RawFd,
        mut buf: IoBuf,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        let op_id = self.allocate_io_op_id();
        let sqe = opcode::Recv::new(types::Fd(fd), buf.as_mut_ptr(), buf.capacity() as u32)
            .build()
            .user_data(op_id.0);
        self.in_flight
            .lock()
            .expect("io_uring in-flight op table poisoned")
            .insert(
                op_id,
                InFlightOp::Recv {
                    buf,
                    handler: Box::new(handler),
                },
            );
        self.submit_or_defer(sqe)?;
        Ok(op_id)
    }

    fn send(&self, fd: RawFd, buf: IoBuf, handler: impl IoCompletionHandler) -> io::Result<IoOpId> {
        let op_id = self.allocate_io_op_id();
        let sqe = opcode::Send::new(types::Fd(fd), buf.as_ptr(), buf.len() as u32)
            .build()
            .user_data(op_id.0);
        self.in_flight
            .lock()
            .expect("io_uring in-flight op table poisoned")
            .insert(
                op_id,
                InFlightOp::Send {
                    buf,
                    handler: Box::new(handler),
                },
            );
        self.submit_or_defer(sqe)?;
        Ok(op_id)
    }

    fn recv_from(
        &self,
        fd: RawFd,
        mut buf: IoBuf,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        let op_id = self.allocate_io_op_id();

        let mut state = Box::new(RecvFromState {
            iov: libc::iovec {
                iov_base: buf.as_mut_ptr().cast(),
                iov_len: buf.capacity(),
            },
            msg: unsafe { std::mem::zeroed() },
            addr: RawSockAddr::empty(),
        });
        state.msg.msg_name = state.addr.as_mut_sockaddr_ptr().cast();
        state.msg.msg_namelen = state.addr.len;
        state.msg.msg_iov = std::ptr::addr_of_mut!(state.iov);
        state.msg.msg_iovlen = 1;

        let sqe = opcode::RecvMsg::new(types::Fd(fd), std::ptr::addr_of_mut!(state.msg))
            .build()
            .user_data(op_id.0);

        self.in_flight
            .lock()
            .expect("io_uring in-flight op table poisoned")
            .insert(
                op_id,
                InFlightOp::RecvFrom {
                    buf,
                    handler: Box::new(handler),
                    state,
                },
            );
        self.submit_or_defer(sqe)?;
        Ok(op_id)
    }

    fn send_to(
        &self,
        fd: RawFd,
        buf: IoBuf,
        target: SocketAddr,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        let op_id = self.allocate_io_op_id();

        let mut state = Box::new(SendToState {
            iov: libc::iovec {
                iov_base: buf.as_ptr() as *mut u8 as *mut libc::c_void,
                iov_len: buf.len(),
            },
            msg: unsafe { std::mem::zeroed() },
            addr: SockAddr::from(target),
        });
        state.msg.msg_name = state.addr.as_ptr() as *mut libc::c_void;
        state.msg.msg_namelen = state.addr.len();
        state.msg.msg_iov = std::ptr::addr_of_mut!(state.iov);
        state.msg.msg_iovlen = 1;

        let sqe = opcode::SendMsg::new(types::Fd(fd), std::ptr::addr_of!(state.msg))
            .build()
            .user_data(op_id.0);

        self.in_flight
            .lock()
            .expect("io_uring in-flight op table poisoned")
            .insert(
                op_id,
                InFlightOp::SendTo {
                    buf,
                    handler: Box::new(handler),
                    _state: state,
                },
            );
        self.submit_or_defer(sqe)?;
        Ok(op_id)
    }

    fn accept(&self, fd: RawFd, handler: impl AcceptCompletionHandler) -> io::Result<IoOpId> {
        let op_id = self.allocate_io_op_id();
        let mut addr = Box::new(RawSockAddr::empty());
        let sqe = opcode::Accept::new(
            types::Fd(fd),
            addr.as_mut_sockaddr_ptr(),
            addr.len_mut_ptr(),
        )
        .build()
        .user_data(op_id.0);
        self.in_flight
            .lock()
            .expect("io_uring in-flight op table poisoned")
            .insert(
                op_id,
                InFlightOp::Accept {
                    handler: Box::new(handler),
                    addr,
                },
            );
        self.submit_or_defer(sqe)?;
        Ok(op_id)
    }

    fn connect(
        &self,
        fd: RawFd,
        target: SocketAddr,
        handler: impl UnitCompletionHandler,
    ) -> io::Result<IoOpId> {
        let op_id = self.allocate_io_op_id();
        let addr = Box::new(SockAddr::from(target));
        let sqe = opcode::Connect::new(types::Fd(fd), addr.as_ptr(), addr.len())
            .build()
            .user_data(op_id.0);
        self.in_flight
            .lock()
            .expect("io_uring in-flight op table poisoned")
            .insert(
                op_id,
                InFlightOp::Connect {
                    handler: Box::new(handler),
                    _addr: addr,
                },
            );
        self.submit_or_defer(sqe)?;
        Ok(op_id)
    }

    fn cancel_io(&self, op: IoOpId) -> io::Result<()> {
        let sqe = opcode::AsyncCancel::new(op.0).build().user_data(WAKE_TOKEN);
        self.submit_or_defer(sqe)
    }
}
