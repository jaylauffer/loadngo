//! `epoll`-backed `CompletionPort`/`ReadinessPort`/`IoPort`, for
//! `target_os = "android"`. Confirmed the *only* viable kernel-async-I/O
//! mechanism available to a real Android app process — `io_uring` is
//! seccomp-blocked for `untrusted_app` on real hardware (Android 14,
//! kernel 5.4.289-qgki), tested directly, see
//! `docs/PROACTOR_ENGINE_ADOPTION.md`'s "Android: `io_uring` is not
//! available to app processes" section for the full writeup.
//!
//! Owns its own independent `epoll_create1` instance — deliberately not
//! built on the NDK's `ALooper_addFd` (which would add this port's fds
//! into whatever `ALooper` the Android framework's own main-thread event
//! pump already owns, a fundamentally different, callback-driven
//! integration model that doesn't fit `CompletionPort::poll`'s
//! caller-blocks-on-this-call contract). Same shape as `KqueuePort::new`
//! calling `kqueue()` for its own independent kqueue fd, not hooking into
//! any OS-provided one.
//!
//! Architecturally mirrors `kqueue.rs` closely (readiness-then-syscall
//! `IoPort` emulation, not true kernel-async I/O the way `uring.rs`'s
//! `IoUringPort` has) with one real difference: kqueue's `EVFILT_READ`/
//! `EVFILT_WRITE` are independent registrations that can coexist for the
//! same fd (two separate `(ident, filter)` kevent entries); epoll
//! registers interest *per fd*, not per direction, so a fd with both a
//! read wait and a write wait outstanding needs one shared registration
//! whose bitmask reflects both. This module tracks that itself (`FdEntry`)
//! and always recomputes the real interest mask on every change rather
//! than relying on `EPOLLONESHOT` — that flag disarms *all* interest for
//! a fd on any single event, not just the direction that fired, which
//! would be wrong the moment two directions are outstanding at once.

use crate::{
    AcceptCompletionHandler, AcceptResult, AcceptTransfer, CompletionEnvelope, CompletionPort,
    IoBuf, IoCompletionHandler, IoOpId, IoPort, IoResult, IoTransfer, PollEvent, ReadinessEvent,
    ReadinessPort, UnitCompletionHandler,
};
use libc::{
    c_int, close, epoll_create1, epoll_ctl, epoll_event, epoll_wait, eventfd, EFD_CLOEXEC,
    EFD_NONBLOCK, EPOLLERR, EPOLLHUP, EPOLLIN, EPOLLOUT, EPOLL_CLOEXEC, EPOLL_CTL_ADD,
    EPOLL_CTL_DEL, EPOLL_CTL_MOD,
};
use socket2::SockAddr;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Sentinel `epoll_event.u64` tags for the two control eventfds, chosen
/// above `u32::MAX` so they can never collide with a real fd (`RawFd` is
/// `i32`, always representable in the low 32 bits — every other
/// registration in this backend tags its `epoll_event` with the raw fd
/// itself, see `sync_interest`). Same purpose as `kqueue.rs`'s
/// `QUEUE_IDENT`/`WAKE_IDENT`, just a different namespace shape: kqueue
/// keeps `EVFILT_USER` idents in their own filter-scoped space entirely
/// separate from fd-keyed filters, so small integers are safe there too.
const QUEUE_TAG: u64 = 1 << 32;
const WAKE_TAG: u64 = 2 << 32;

/// What an `IoPort` op that lost the "try immediately" race (got
/// `EAGAIN`/`EWOULDBLOCK`, or — for `connect` — `EINPROGRESS`) needs kept
/// until epoll reports its fd ready. Identical shape to `kqueue.rs`'s
/// `InFlightOp` — the real `read`/`recv`/`recvfrom`/etc. syscalls this
/// resolves against are the same libc calls on Android/bionic as on
/// macOS/BSD.
enum InFlightOp {
    Read {
        fd: RawFd,
        buf: IoBuf,
        offset: u64,
        handler: Box<dyn IoCompletionHandler>,
    },
    Write {
        fd: RawFd,
        buf: IoBuf,
        offset: u64,
        handler: Box<dyn IoCompletionHandler>,
    },
    Recv {
        fd: RawFd,
        buf: IoBuf,
        handler: Box<dyn IoCompletionHandler>,
    },
    Send {
        fd: RawFd,
        buf: IoBuf,
        handler: Box<dyn IoCompletionHandler>,
    },
    RecvFrom {
        fd: RawFd,
        buf: IoBuf,
        handler: Box<dyn IoCompletionHandler>,
    },
    SendTo {
        fd: RawFd,
        buf: IoBuf,
        target: SocketAddr,
        handler: Box<dyn IoCompletionHandler>,
    },
    Accept {
        fd: RawFd,
        handler: Box<dyn AcceptCompletionHandler>,
    },
    /// Waiting on writability — the standard BSD-sockets non-blocking
    /// connect pattern: once writable, `getsockopt(SO_ERROR)` gives the
    /// real result. Same as `kqueue.rs`'s `EVFILT_WRITE`-based `Connect`.
    Connect {
        fd: RawFd,
        handler: Box<dyn UnitCompletionHandler>,
    },
}

/// What's currently registered for one fd's read side: either a standing
/// `ReadinessPort` registration (persists across events, cleared only by
/// `deregister`) or a one-shot `IoPort` op waiting on `EPOLLIN` (cleared
/// the moment it resolves). `Copy`/`Clone` so `resolve_fd_event` can read
/// it out of the `fds` table and release the lock before doing anything
/// else with it.
#[derive(Clone, Copy)]
enum ReadSide {
    IoOp(IoOpId),
    Readiness(u64),
}

#[derive(Default)]
struct FdEntry {
    read: Option<ReadSide>,
    write: Option<IoOpId>,
}

impl FdEntry {
    fn interest_mask(&self) -> u32 {
        let mut mask = 0u32;
        if self.read.is_some() {
            mask |= EPOLLIN as u32;
        }
        if self.write.is_some() {
            mask |= EPOLLOUT as u32;
        }
        mask
    }

    fn is_empty(&self) -> bool {
        self.read.is_none() && self.write.is_none()
    }
}

pub struct EpollPort {
    epfd: c_int,
    queue_fd: c_int,
    wake_fd: c_int,
    queue: Mutex<VecDeque<CompletionEnvelope>>,
    /// Pre-resolved `IoPort` completions — either an op that succeeded
    /// (or failed) immediately on its first, optimistic non-blocking
    /// attempt (no epoll round-trip needed at all), or one that
    /// `cancel_io` synchronously cancelled. Drained the same way `queue`
    /// is, via the same `QUEUE_TAG` wake. Same role as `kqueue.rs`'s
    /// identically-named field.
    io_completions: Mutex<VecDeque<Box<dyn FnOnce() + Send>>>,
    in_flight: Mutex<HashMap<IoOpId, InFlightOp>>,
    fds: Mutex<HashMap<RawFd, FdEntry>>,
    next_io_op_id: AtomicU64,
}

impl EpollPort {
    pub fn new() -> io::Result<Self> {
        let epfd = unsafe { epoll_create1(EPOLL_CLOEXEC) };
        if epfd == -1 {
            return Err(io::Error::last_os_error());
        }
        let queue_fd = unsafe { eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC) };
        if queue_fd == -1 {
            let err = io::Error::last_os_error();
            unsafe {
                close(epfd);
            }
            return Err(err);
        }
        let wake_fd = unsafe { eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC) };
        if wake_fd == -1 {
            let err = io::Error::last_os_error();
            unsafe {
                close(queue_fd);
                close(epfd);
            }
            return Err(err);
        }

        // `port` owns all three fds from here on — an error below drops
        // it, and `Drop for EpollPort` closes them, so no manual cleanup
        // is needed past this point.
        let port = Self {
            epfd,
            queue_fd,
            wake_fd,
            queue: Mutex::new(VecDeque::new()),
            io_completions: Mutex::new(VecDeque::new()),
            in_flight: Mutex::new(HashMap::new()),
            fds: Mutex::new(HashMap::new()),
            next_io_op_id: AtomicU64::new(0),
        };
        port.register_control_fd(queue_fd, QUEUE_TAG)?;
        port.register_control_fd(wake_fd, WAKE_TAG)?;
        Ok(port)
    }

    fn register_control_fd(&self, fd: c_int, tag: u64) -> io::Result<()> {
        let mut event = epoll_event {
            events: EPOLLIN as u32,
            u64: tag,
        };
        let result = unsafe { epoll_ctl(self.epfd, EPOLL_CTL_ADD, fd, &mut event) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn drain_completion(&self) -> Option<CompletionEnvelope> {
        self.queue
            .lock()
            .expect("epoll completion queue poisoned")
            .pop_front()
    }

    fn drain_io_completion(&self) -> Option<Box<dyn FnOnce() + Send>> {
        self.io_completions
            .lock()
            .expect("epoll io-completion queue poisoned")
            .pop_front()
    }

    fn allocate_io_op_id(&self) -> IoOpId {
        // No tag bit needed here, unlike kqueue.rs's IO_OP_TAG: this
        // backend never looks an op up by a raw integer shared with
        // readiness tokens — `resolve_fd_event` always goes through
        // `FdEntry`'s typed `ReadSide` enum instead, so there's nothing
        // for a tag bit to disambiguate.
        IoOpId(self.next_io_op_id.fetch_add(1, Ordering::Relaxed))
    }

    fn queue_io_completion(&self, thunk: Box<dyn FnOnce() + Send>) -> io::Result<()> {
        self.io_completions
            .lock()
            .expect("epoll io-completion queue poisoned")
            .push_back(thunk);
        self.trigger(self.queue_fd)
    }

    fn trigger(&self, fd: c_int) -> io::Result<()> {
        let value: u64 = 1;
        let result = unsafe { libc::write(fd, (&raw const value).cast(), 8) };
        if result == -1 {
            let err = io::Error::last_os_error();
            // EAGAIN here just means the eventfd's counter is already
            // non-zero (an earlier trigger hasn't been drained yet) — the
            // reader still observes it as readable, so this isn't a real
            // failure, just a redundant wake.
            if err.kind() == io::ErrorKind::WouldBlock {
                return Ok(());
            }
            return Err(err);
        }
        Ok(())
    }

    /// Drains a control eventfd's counter back to zero. Necessary because
    /// these are registered level-triggered: without this, epoll would
    /// keep reporting the fd ready on every subsequent `poll()` call
    /// forever after the first trigger, not just once.
    fn drain_eventfd(fd: c_int) {
        let mut value: u64 = 0;
        // A concurrent trigger() can race this read and re-set the
        // counter to non-zero right after — harmless either way, the
        // next poll() simply sees it ready again.
        unsafe {
            libc::read(fd, (&raw mut value).cast(), 8);
        }
    }

    /// Sets `O_NONBLOCK` on `fd`. Necessary, not just a convenience: once
    /// epoll reports a fd ready, this backend performs the real syscall
    /// right there on the poll thread — if the fd were still blocking, a
    /// spurious wakeup or a race where something else already consumed
    /// the data would make that syscall genuinely block, stalling every
    /// other operation sharing this proactor. Identical to `kqueue.rs`'s
    /// same-named helper.
    fn set_nonblocking(fd: RawFd) -> io::Result<()> {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags == -1 {
            return Err(io::Error::last_os_error());
        }
        if flags & libc::O_NONBLOCK != 0 {
            return Ok(());
        }
        let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Recomputes and applies `fd`'s epoll registration from its current
    /// `FdEntry` state: `ADD` the first time this fd is ever registered,
    /// `MOD` to change bits on an existing registration, `DEL` once
    /// neither direction is wanted any more. Always driven from a fresh
    /// snapshot taken under `fds`'s lock, never from `EPOLLONESHOT`'s
    /// implicit disarm — see the module doc for why that flag is wrong
    /// here.
    fn sync_interest(&self, fd: RawFd, is_new: bool, empty: bool, mask: u32) -> io::Result<()> {
        if empty {
            let result = unsafe { epoll_ctl(self.epfd, EPOLL_CTL_DEL, fd, std::ptr::null_mut()) };
            if result == -1 {
                let err = io::Error::last_os_error();
                // ENOENT means it was never actually registered with
                // epoll in the first place (e.g. an immediately-resolved
                // op that never needed a real registration) — not a
                // failure.
                if err.raw_os_error() != Some(libc::ENOENT) {
                    return Err(err);
                }
            }
            return Ok(());
        }
        let op = if is_new { EPOLL_CTL_ADD } else { EPOLL_CTL_MOD };
        let mut event = epoll_event {
            events: mask,
            u64: fd as u64,
        };
        let result = unsafe { epoll_ctl(self.epfd, op, fd, &mut event) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn arm_read(&self, fd: RawFd, side: ReadSide) -> io::Result<()> {
        let (is_new, mask) = {
            let mut fds = self.fds.lock().expect("epoll fd table poisoned");
            let is_new = !fds.contains_key(&fd);
            let entry = fds.entry(fd).or_default();
            entry.read = Some(side);
            (is_new, entry.interest_mask())
        };
        self.sync_interest(fd, is_new, false, mask)
    }

    fn arm_write(&self, fd: RawFd, op_id: IoOpId) -> io::Result<()> {
        let (is_new, mask) = {
            let mut fds = self.fds.lock().expect("epoll fd table poisoned");
            let is_new = !fds.contains_key(&fd);
            let entry = fds.entry(fd).or_default();
            entry.write = Some(op_id);
            (is_new, entry.interest_mask())
        };
        self.sync_interest(fd, is_new, false, mask)
    }

    fn clear_read(&self, fd: RawFd) -> io::Result<()> {
        let (empty, mask) = {
            let mut fds = self.fds.lock().expect("epoll fd table poisoned");
            let Some(entry) = fds.get_mut(&fd) else {
                return Ok(());
            };
            entry.read = None;
            let empty = entry.is_empty();
            let mask = entry.interest_mask();
            if empty {
                fds.remove(&fd);
            }
            (empty, mask)
        };
        self.sync_interest(fd, false, empty, mask)
    }

    fn clear_write(&self, fd: RawFd) -> io::Result<()> {
        let (empty, mask) = {
            let mut fds = self.fds.lock().expect("epoll fd table poisoned");
            let Some(entry) = fds.get_mut(&fd) else {
                return Ok(());
            };
            entry.write = None;
            let empty = entry.is_empty();
            let mask = entry.interest_mask();
            if empty {
                fds.remove(&fd);
            }
            (empty, mask)
        };
        self.sync_interest(fd, false, empty, mask)
    }

    /// Looks up and removes `op_id`'s `InFlightOp` and performs the real
    /// syscall it's been waiting to make, now that epoll reported its fd
    /// ready. Returns a benign `PollEvent::Wake` for an `op_id` with no
    /// matching entry — shouldn't happen in practice, but a stray event
    /// (e.g. racing a cancellation) is far better tolerated than
    /// panicking on it. Identical body to `kqueue.rs`'s
    /// `resolve_io_readiness` — the syscalls are the same libc calls on
    /// Android/bionic as on macOS/BSD.
    fn resolve_io_op(&self, op_id: IoOpId) -> PollEvent {
        let entry = self
            .in_flight
            .lock()
            .expect("epoll in-flight op table poisoned")
            .remove(&op_id);
        let Some(entry) = entry else {
            return PollEvent::Wake;
        };

        let thunk: Box<dyn FnOnce() + Send> = match entry {
            InFlightOp::Read {
                fd,
                mut buf,
                offset,
                handler,
            } => {
                let rc = unsafe {
                    libc::pread(
                        fd,
                        buf.as_mut_ptr().cast(),
                        buf.capacity(),
                        offset as libc::off_t,
                    )
                };
                let io_result = Self::finish_read_like(rc, buf);
                Box::new(move || handler.run(io_result))
            }
            InFlightOp::Write {
                fd,
                buf,
                offset,
                handler,
            } => {
                let rc = unsafe {
                    libc::pwrite(fd, buf.as_ptr().cast(), buf.len(), offset as libc::off_t)
                };
                let io_result = Self::finish_write_like(rc, buf);
                Box::new(move || handler.run(io_result))
            }
            InFlightOp::Recv {
                fd,
                mut buf,
                handler,
            } => {
                let rc = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.capacity(), 0) };
                let io_result = Self::finish_read_like(rc, buf);
                Box::new(move || handler.run(io_result))
            }
            InFlightOp::Send { fd, buf, handler } => {
                let rc = unsafe { libc::send(fd, buf.as_ptr().cast(), buf.len(), 0) };
                let io_result = Self::finish_write_like(rc, buf);
                Box::new(move || handler.run(io_result))
            }
            InFlightOp::RecvFrom {
                fd,
                mut buf,
                handler,
            } => {
                let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
                let mut len = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
                let rc = unsafe {
                    libc::recvfrom(
                        fd,
                        buf.as_mut_ptr().cast(),
                        buf.capacity(),
                        0,
                        std::ptr::addr_of_mut!(storage).cast(),
                        &mut len,
                    )
                };
                let peer = unsafe { SockAddr::new(storage, len) }.as_socket();
                let io_result = if rc >= 0 {
                    let mut buf = buf;
                    unsafe { buf.set_filled_len(rc as usize) };
                    Ok(IoTransfer {
                        buf,
                        bytes_transferred: rc as u32,
                        peer,
                    })
                } else {
                    Err(io::Error::last_os_error())
                };
                Box::new(move || handler.run(io_result))
            }
            InFlightOp::SendTo {
                fd,
                buf,
                target,
                handler,
            } => {
                let addr = SockAddr::from(target);
                let rc = unsafe {
                    libc::sendto(
                        fd,
                        buf.as_ptr().cast(),
                        buf.len(),
                        0,
                        addr.as_ptr(),
                        addr.len(),
                    )
                };
                let io_result = Self::finish_write_like(rc, buf);
                Box::new(move || handler.run(io_result))
            }
            InFlightOp::Accept { fd, handler } => {
                let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
                let mut len = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
                let new_fd =
                    unsafe { libc::accept(fd, std::ptr::addr_of_mut!(storage).cast(), &mut len) };
                let accept_result: AcceptResult = if new_fd >= 0 {
                    match unsafe { SockAddr::new(storage, len) }.as_socket() {
                        Some(peer) => Ok(AcceptTransfer { new_fd, peer }),
                        None => Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "accept completed but the peer address family was unrecognized",
                        )),
                    }
                } else {
                    Err(io::Error::last_os_error())
                };
                Box::new(move || handler.run(accept_result))
            }
            InFlightOp::Connect { fd, handler } => {
                let mut err_val: libc::c_int = 0;
                let mut err_len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                let rc = unsafe {
                    libc::getsockopt(
                        fd,
                        libc::SOL_SOCKET,
                        libc::SO_ERROR,
                        std::ptr::addr_of_mut!(err_val).cast(),
                        &mut err_len,
                    )
                };
                let unit_result = if rc == -1 {
                    Err(io::Error::last_os_error())
                } else if err_val != 0 {
                    Err(io::Error::from_raw_os_error(err_val))
                } else {
                    Ok(())
                };
                Box::new(move || handler.run(unit_result))
            }
        };

        PollEvent::IoCompletion(thunk)
    }

    /// Resolves whichever side(s) `bits` (this fd's reported
    /// `epoll_event.events`) indicate are ready, preferring the read side
    /// when both are set in the same event — `epoll_wait`'s `maxevents =
    /// 1` here means only one `PollEvent` can be returned per call, and
    /// leaving the other side's registration untouched (still armed,
    /// level-triggered) means the very next `poll()` call reports it
    /// again on its own, so nothing is lost by resolving one at a time.
    /// `EPOLLERR`/`EPOLLHUP` unblock *both* directions, matching the
    /// common epoll convention (the error/hangup is only really
    /// observable via the next real syscall's own return value).
    fn resolve_fd_event(&self, fd: RawFd, bits: u32) -> io::Result<PollEvent> {
        let error_or_hup = bits & (EPOLLERR as u32 | EPOLLHUP as u32) != 0;
        let readable = error_or_hup || bits & EPOLLIN as u32 != 0;
        let writable = error_or_hup || bits & EPOLLOUT as u32 != 0;

        if readable {
            let side = {
                let fds = self.fds.lock().expect("epoll fd table poisoned");
                fds.get(&fd).and_then(|entry| entry.read)
            };
            if let Some(side) = side {
                self.clear_read(fd)?;
                return Ok(match side {
                    ReadSide::IoOp(op_id) => self.resolve_io_op(op_id),
                    ReadSide::Readiness(token) => PollEvent::Readiness(ReadinessEvent { token }),
                });
            }
        }
        if writable {
            let op_id = {
                let fds = self.fds.lock().expect("epoll fd table poisoned");
                fds.get(&fd).and_then(|entry| entry.write)
            };
            if let Some(op_id) = op_id {
                self.clear_write(fd)?;
                return Ok(self.resolve_io_op(op_id));
            }
        }
        // Stray event with nothing left registered for either direction
        // (e.g. a duplicate wakeup racing a cancellation) — benign,
        // matches kqueue.rs's resolve_io_readiness returning
        // PollEvent::Wake for an unmatched op_id.
        Ok(PollEvent::Wake)
    }

    /// Interprets a `read`/`recv`/`recvfrom`-style syscall's return value
    /// (bytes read on success, `-1` with `errno` set on failure), calling
    /// `io::Error::last_os_error()` immediately — callers must not run
    /// any other libc call between the syscall and this, or errno could
    /// be clobbered first. Identical to `kqueue.rs`'s same-named helper.
    fn finish_read_like(rc: isize, buf: IoBuf) -> IoResult {
        if rc >= 0 {
            let mut buf = buf;
            unsafe { buf.set_filled_len(rc as usize) };
            Ok(IoTransfer {
                buf,
                bytes_transferred: rc as u32,
                peer: None,
            })
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Same as `finish_read_like`, for `write`/`send`/`sendto`-style
    /// syscalls, which don't modify the buffer's contents or length.
    fn finish_write_like(rc: isize, buf: IoBuf) -> IoResult {
        if rc >= 0 {
            Ok(IoTransfer {
                buf,
                bytes_transferred: rc as u32,
                peer: None,
            })
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn would_block(err: &io::Error) -> bool {
        // Not a `matches!` OR-pattern: EAGAIN == EWOULDBLOCK on every
        // platform this backend targets, which would make one arm an
        // unreachable-pattern warning — this form stays correct (if
        // redundant) even if that ever stopped being true somewhere.
        // Identical to kqueue.rs's same-named helper.
        let code = err.raw_os_error();
        code == Some(libc::EAGAIN) || code == Some(libc::EWOULDBLOCK)
    }
}

impl CompletionPort for EpollPort {
    fn post(&self, envelope: CompletionEnvelope) -> io::Result<()> {
        self.queue
            .lock()
            .expect("epoll completion queue poisoned")
            .push_back(envelope);
        self.trigger(self.queue_fd)
    }

    fn poll(&self, timeout: Option<Duration>) -> io::Result<PollEvent> {
        if let Some(envelope) = self.drain_completion() {
            return Ok(PollEvent::Completion(envelope));
        }
        if let Some(thunk) = self.drain_io_completion() {
            return Ok(PollEvent::IoCompletion(thunk));
        }

        let mut event = epoll_event { events: 0, u64: 0 };
        let timeout_ms: c_int = match timeout {
            None => -1,
            Some(duration) => {
                // epoll_wait's timeout is a plain millisecond c_int;
                // clamp rather than overflow for a pathologically large
                // deferred deadline.
                i32::try_from(duration.as_millis()).unwrap_or(i32::MAX)
            }
        };

        // Retries on EINTR: a stray signal delivered to this thread
        // during the blocking epoll_wait() call must not be allowed to
        // propagate as an error here — see uring.rs/kqueue.rs's matching
        // fix (found via a live strace attach on proactor-harness's
        // throughput bench) for why. Retrying is correct and doesn't
        // need to resubmit anything: epoll_wait has no separate
        // submission step, so nothing could have partially landed.
        let result = loop {
            let attempt = unsafe { epoll_wait(self.epfd, &mut event, 1, timeout_ms) };
            if attempt == -1 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            break attempt;
        };
        if result == 0 {
            return Ok(PollEvent::Timeout);
        }

        let tag = event.u64;
        let bits = event.events;

        if tag == QUEUE_TAG {
            Self::drain_eventfd(self.queue_fd);
            if let Some(envelope) = self.drain_completion() {
                return Ok(PollEvent::Completion(envelope));
            }
            if let Some(thunk) = self.drain_io_completion() {
                return Ok(PollEvent::IoCompletion(thunk));
            }
            return Ok(PollEvent::Wake);
        }
        if tag == WAKE_TAG {
            Self::drain_eventfd(self.wake_fd);
            return Ok(PollEvent::Wake);
        }

        // Every other registration in this backend tags its epoll_event
        // with the raw fd itself (see sync_interest) — RawFd is i32, so
        // this round-trips exactly for every real fd, which is always
        // non-negative.
        let fd = tag as RawFd;
        self.resolve_fd_event(fd, bits)
    }

    fn wake(&self) -> io::Result<()> {
        self.trigger(self.wake_fd)
    }

    fn begin_shutdown(&self) {
        let op_ids: Vec<IoOpId> = self
            .in_flight
            .lock()
            .expect("epoll in-flight op table poisoned")
            .keys()
            .copied()
            .collect();
        for op_id in op_ids {
            // Best-effort, synchronous: unlike IoUringPort's cancel_io,
            // this doesn't need to wait for any kernel-side confirmation
            // — epoll_ctl(EPOLL_CTL_DEL) takes effect immediately, so
            // shutdown_complete() becomes true right after this loop
            // finishes, not after some later poll() cycle. Same as
            // kqueue.rs's begin_shutdown.
            let _ = self.cancel_io(op_id);
        }
    }

    fn shutdown_complete(&self) -> bool {
        self.in_flight
            .lock()
            .expect("epoll in-flight op table poisoned")
            .is_empty()
    }
}

impl ReadinessPort for EpollPort {
    fn register_readable(&self, fd: RawFd, token: u64) -> io::Result<()> {
        self.arm_read(fd, ReadSide::Readiness(token))
    }

    fn deregister(&self, fd: RawFd) -> io::Result<()> {
        self.clear_read(fd)
    }
}

impl IoPort for EpollPort {
    fn read(
        &self,
        fd: RawFd,
        mut buf: IoBuf,
        offset: u64,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        Self::set_nonblocking(fd)?;
        let handler: Box<dyn IoCompletionHandler> = Box::new(handler);
        let rc = unsafe {
            libc::pread(
                fd,
                buf.as_mut_ptr().cast(),
                buf.capacity(),
                offset as libc::off_t,
            )
        };
        let err = io::Error::last_os_error();
        if rc >= 0 || !Self::would_block(&err) {
            let io_result = Self::finish_read_like(rc, buf);
            let op_id = self.allocate_io_op_id();
            self.queue_io_completion(Box::new(move || handler.run(io_result)))?;
            return Ok(op_id);
        }
        let op_id = self.allocate_io_op_id();
        self.in_flight
            .lock()
            .expect("epoll in-flight op table poisoned")
            .insert(
                op_id,
                InFlightOp::Read {
                    fd,
                    buf,
                    offset,
                    handler,
                },
            );
        self.arm_read(fd, ReadSide::IoOp(op_id))?;
        Ok(op_id)
    }

    fn write(
        &self,
        fd: RawFd,
        buf: IoBuf,
        offset: u64,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        Self::set_nonblocking(fd)?;
        let handler: Box<dyn IoCompletionHandler> = Box::new(handler);
        let rc = unsafe { libc::pwrite(fd, buf.as_ptr().cast(), buf.len(), offset as libc::off_t) };
        let err = io::Error::last_os_error();
        if rc >= 0 || !Self::would_block(&err) {
            let io_result = Self::finish_write_like(rc, buf);
            let op_id = self.allocate_io_op_id();
            self.queue_io_completion(Box::new(move || handler.run(io_result)))?;
            return Ok(op_id);
        }
        let op_id = self.allocate_io_op_id();
        self.in_flight
            .lock()
            .expect("epoll in-flight op table poisoned")
            .insert(
                op_id,
                InFlightOp::Write {
                    fd,
                    buf,
                    offset,
                    handler,
                },
            );
        self.arm_write(fd, op_id)?;
        Ok(op_id)
    }

    fn recv(
        &self,
        fd: RawFd,
        mut buf: IoBuf,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        Self::set_nonblocking(fd)?;
        let handler: Box<dyn IoCompletionHandler> = Box::new(handler);
        let rc = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.capacity(), 0) };
        let err = io::Error::last_os_error();
        if rc >= 0 || !Self::would_block(&err) {
            let io_result = Self::finish_read_like(rc, buf);
            let op_id = self.allocate_io_op_id();
            self.queue_io_completion(Box::new(move || handler.run(io_result)))?;
            return Ok(op_id);
        }
        let op_id = self.allocate_io_op_id();
        self.in_flight
            .lock()
            .expect("epoll in-flight op table poisoned")
            .insert(op_id, InFlightOp::Recv { fd, buf, handler });
        self.arm_read(fd, ReadSide::IoOp(op_id))?;
        Ok(op_id)
    }

    fn send(&self, fd: RawFd, buf: IoBuf, handler: impl IoCompletionHandler) -> io::Result<IoOpId> {
        Self::set_nonblocking(fd)?;
        let handler: Box<dyn IoCompletionHandler> = Box::new(handler);
        let rc = unsafe { libc::send(fd, buf.as_ptr().cast(), buf.len(), 0) };
        let err = io::Error::last_os_error();
        if rc >= 0 || !Self::would_block(&err) {
            let io_result = Self::finish_write_like(rc, buf);
            let op_id = self.allocate_io_op_id();
            self.queue_io_completion(Box::new(move || handler.run(io_result)))?;
            return Ok(op_id);
        }
        let op_id = self.allocate_io_op_id();
        self.in_flight
            .lock()
            .expect("epoll in-flight op table poisoned")
            .insert(op_id, InFlightOp::Send { fd, buf, handler });
        self.arm_write(fd, op_id)?;
        Ok(op_id)
    }

    fn recv_from(
        &self,
        fd: RawFd,
        mut buf: IoBuf,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        Self::set_nonblocking(fd)?;
        let handler: Box<dyn IoCompletionHandler> = Box::new(handler);
        let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        let rc = unsafe {
            libc::recvfrom(
                fd,
                buf.as_mut_ptr().cast(),
                buf.capacity(),
                0,
                std::ptr::addr_of_mut!(storage).cast(),
                &mut len,
            )
        };
        let err = io::Error::last_os_error();
        if rc >= 0 || !Self::would_block(&err) {
            let peer = unsafe { SockAddr::new(storage, len) }.as_socket();
            let io_result: IoResult = if rc >= 0 {
                unsafe { buf.set_filled_len(rc as usize) };
                Ok(IoTransfer {
                    buf,
                    bytes_transferred: rc as u32,
                    peer,
                })
            } else {
                Err(err)
            };
            let op_id = self.allocate_io_op_id();
            self.queue_io_completion(Box::new(move || handler.run(io_result)))?;
            return Ok(op_id);
        }
        let op_id = self.allocate_io_op_id();
        self.in_flight
            .lock()
            .expect("epoll in-flight op table poisoned")
            .insert(op_id, InFlightOp::RecvFrom { fd, buf, handler });
        self.arm_read(fd, ReadSide::IoOp(op_id))?;
        Ok(op_id)
    }

    fn send_to(
        &self,
        fd: RawFd,
        buf: IoBuf,
        target: SocketAddr,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        Self::set_nonblocking(fd)?;
        let handler: Box<dyn IoCompletionHandler> = Box::new(handler);
        let addr = SockAddr::from(target);
        let rc = unsafe {
            libc::sendto(
                fd,
                buf.as_ptr().cast(),
                buf.len(),
                0,
                addr.as_ptr(),
                addr.len(),
            )
        };
        let err = io::Error::last_os_error();
        if rc >= 0 || !Self::would_block(&err) {
            let io_result = Self::finish_write_like(rc, buf);
            let op_id = self.allocate_io_op_id();
            self.queue_io_completion(Box::new(move || handler.run(io_result)))?;
            return Ok(op_id);
        }
        let op_id = self.allocate_io_op_id();
        self.in_flight
            .lock()
            .expect("epoll in-flight op table poisoned")
            .insert(
                op_id,
                InFlightOp::SendTo {
                    fd,
                    buf,
                    target,
                    handler,
                },
            );
        self.arm_write(fd, op_id)?;
        Ok(op_id)
    }

    fn accept(&self, fd: RawFd, handler: impl AcceptCompletionHandler) -> io::Result<IoOpId> {
        Self::set_nonblocking(fd)?;
        let handler: Box<dyn AcceptCompletionHandler> = Box::new(handler);
        let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        let new_fd = unsafe { libc::accept(fd, std::ptr::addr_of_mut!(storage).cast(), &mut len) };
        let err = io::Error::last_os_error();
        if new_fd >= 0 || !Self::would_block(&err) {
            let accept_result: AcceptResult = if new_fd >= 0 {
                match unsafe { SockAddr::new(storage, len) }.as_socket() {
                    Some(peer) => Ok(AcceptTransfer { new_fd, peer }),
                    None => Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "accept completed but the peer address family was unrecognized",
                    )),
                }
            } else {
                Err(err)
            };
            let op_id = self.allocate_io_op_id();
            self.queue_io_completion(Box::new(move || handler.run(accept_result)))?;
            return Ok(op_id);
        }
        let op_id = self.allocate_io_op_id();
        self.in_flight
            .lock()
            .expect("epoll in-flight op table poisoned")
            .insert(op_id, InFlightOp::Accept { fd, handler });
        self.arm_read(fd, ReadSide::IoOp(op_id))?;
        Ok(op_id)
    }

    fn connect(
        &self,
        fd: RawFd,
        target: SocketAddr,
        handler: impl UnitCompletionHandler,
    ) -> io::Result<IoOpId> {
        Self::set_nonblocking(fd)?;
        let handler: Box<dyn UnitCompletionHandler> = Box::new(handler);
        let addr = SockAddr::from(target);
        let rc = unsafe { libc::connect(fd, addr.as_ptr(), addr.len()) };
        if rc == 0 {
            let op_id = self.allocate_io_op_id();
            self.queue_io_completion(Box::new(move || handler.run(Ok(()))))?;
            return Ok(op_id);
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EINPROGRESS) {
            let op_id = self.allocate_io_op_id();
            self.queue_io_completion(Box::new(move || handler.run(Err(err))))?;
            return Ok(op_id);
        }
        let op_id = self.allocate_io_op_id();
        self.in_flight
            .lock()
            .expect("epoll in-flight op table poisoned")
            .insert(op_id, InFlightOp::Connect { fd, handler });
        self.arm_write(fd, op_id)?;
        Ok(op_id)
    }

    fn cancel_io(&self, op: IoOpId) -> io::Result<()> {
        let entry = self
            .in_flight
            .lock()
            .expect("epoll in-flight op table poisoned")
            .remove(&op);
        let Some(entry) = entry else {
            return Ok(());
        };

        let (fd, is_read_side) = match &entry {
            InFlightOp::Read { fd, .. }
            | InFlightOp::Recv { fd, .. }
            | InFlightOp::RecvFrom { fd, .. }
            | InFlightOp::Accept { fd, .. } => (*fd, true),
            InFlightOp::Write { fd, .. }
            | InFlightOp::Send { fd, .. }
            | InFlightOp::SendTo { fd, .. }
            | InFlightOp::Connect { fd, .. } => (*fd, false),
        };
        // Best-effort: if the registration already fired (racing this
        // cancellation) there's nothing left to clear, which is fine —
        // the entry's already been removed above either way. Matches
        // kqueue.rs's cancel_io tolerance for the same race.
        let _ = if is_read_side {
            self.clear_read(fd)
        } else {
            self.clear_write(fd)
        };

        let cancelled_err = || io::Error::new(io::ErrorKind::Interrupted, "operation cancelled");
        let thunk: Box<dyn FnOnce() + Send> = match entry {
            InFlightOp::Read { handler, .. }
            | InFlightOp::Recv { handler, .. }
            | InFlightOp::RecvFrom { handler, .. }
            | InFlightOp::Write { handler, .. }
            | InFlightOp::Send { handler, .. }
            | InFlightOp::SendTo { handler, .. } => {
                Box::new(move || handler.run(Err(cancelled_err())))
            }
            InFlightOp::Accept { handler, .. } => {
                Box::new(move || handler.run(Err(cancelled_err())))
            }
            InFlightOp::Connect { handler, .. } => {
                Box::new(move || handler.run(Err(cancelled_err())))
            }
        };
        self.queue_io_completion(thunk)
    }
}

impl Drop for EpollPort {
    fn drop(&mut self) {
        unsafe {
            close(self.wake_fd);
            close(self.queue_fd);
            close(self.epfd);
        }
    }
}
