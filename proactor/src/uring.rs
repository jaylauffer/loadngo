use crate::{CompletionEnvelope, CompletionPort, PollEvent, ReadinessEvent, ReadinessPort};
use io_uring::{opcode, types, IoUring};
use libc::{timespec, POLLERR, POLLHUP, POLLIN, POLLRDHUP};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::os::fd::RawFd;
use std::sync::Mutex;
use std::time::Duration;

const QUEUE_TOKEN: u64 = 1;
const WAKE_TOKEN: u64 = 2;
const MAX_EVENTS: usize = 256;
const READINESS_POLL_MASK: u32 = (POLLIN | POLLERR | POLLHUP | POLLRDHUP) as u32;

/// A ring-mutating operation requested by `register_readable`/`deregister`,
/// deferred until the next time `poll()` actually holds the `ring` lock
/// (see the module-level note on `pending_ops` for why this can't just
/// lock `ring` directly).
enum PendingRingOp {
    Register { fd: RawFd, token: u64 },
    Deregister { token: u64 },
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
        })
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

        // Wait for events
        let result = if let Some(duration) = timeout {
            let ts = Self::duration_to_timespec(duration);
            let uring_ts = types::Timespec::new()
                .sec(ts.tv_sec as _)
                .nsec(ts.tv_nsec as _);
            let args = types::SubmitArgs::new().timespec(&uring_ts);
            ring.submitter().submit_with_args(1, &args)
        } else {
            ring.submit_and_wait(1)
        };

        match result {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {
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
