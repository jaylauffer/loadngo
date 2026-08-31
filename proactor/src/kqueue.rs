use crate::{
    AcceptCompletionHandler, AcceptResult, AcceptTransfer, CompletionEnvelope, CompletionPort,
    IoBuf, IoCompletionHandler, IoOpId, IoPort, IoResult, IoTransfer, PollEvent, ReadinessEvent,
    ReadinessPort, UnitCompletionHandler,
};
use libc::{
    c_int, close, kevent, kqueue, timespec, EVFILT_READ, EVFILT_USER, EVFILT_WRITE, EV_ADD,
    EV_CLEAR, EV_DELETE, EV_ENABLE, EV_ERROR, EV_ONESHOT, EV_RECEIPT, NOTE_TRIGGER,
};
use socket2::SockAddr;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::mem::MaybeUninit;
use std::net::SocketAddr;
use std::os::fd::RawFd;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

const QUEUE_IDENT: usize = 1;
const WAKE_IDENT: usize = 2;
/// Same tagging scheme as `uring.rs`'s `IO_OP_TAG` -- see that constant's
/// doc for why (`register_readable` tokens on the same port instance must
/// stay below 2^63, true of every real caller).
const IO_OP_TAG: u64 = 1 << 63;

/// What an `IoPort` op that lost the "try immediately" race (got
/// `EAGAIN`/`EWOULDBLOCK`, or -- for `connect` -- `EINPROGRESS`) needs
/// kept until its one-shot kqueue registration fires. Unlike
/// `uring.rs`'s `InFlightOp`, this never needs to pin an `iovec`/`msghdr`
/// across an async kernel boundary: kqueue only ever tells us "you may
/// now perform a syscall", and the actual `read`/`recv`/`recvfrom`/etc.
/// happens synchronously, entirely on the stack, once we're ready to
/// perform it -- so there's nothing here for a raw pointer to outlive.
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
    /// Waiting on `EVFILT_WRITE` -- the standard BSD-sockets non-blocking
    /// connect pattern: once writable, `getsockopt(SO_ERROR)` gives the
    /// real result.
    Connect {
        fd: RawFd,
        handler: Box<dyn UnitCompletionHandler>,
    },
}

pub struct KqueuePort {
    kq: c_int,
    queue: Mutex<VecDeque<CompletionEnvelope>>,
    /// Pre-resolved `IoPort` completions -- either an op that succeeded
    /// (or failed) immediately on its first, optimistic non-blocking
    /// attempt (no kqueue round-trip needed at all), or one that
    /// `cancel_io` synchronously cancelled. Drained the same way `queue`
    /// is, via the same `QUEUE_IDENT` wake.
    io_completions: Mutex<VecDeque<Box<dyn FnOnce() + Send>>>,
    in_flight: Mutex<HashMap<IoOpId, InFlightOp>>,
    next_io_op_id: AtomicU64,
}

impl KqueuePort {
    pub fn new() -> io::Result<Self> {
        let kq = unsafe { kqueue() };
        if kq == -1 {
            return Err(io::Error::last_os_error());
        }

        let port = Self {
            kq,
            queue: Mutex::new(VecDeque::new()),
            io_completions: Mutex::new(VecDeque::new()),
            in_flight: Mutex::new(HashMap::new()),
            next_io_op_id: AtomicU64::new(0),
        };
        if let Err(err) = port.register_user_event(QUEUE_IDENT) {
            unsafe {
                close(kq);
            }
            return Err(err);
        }
        if let Err(err) = port.register_user_event(WAKE_IDENT) {
            unsafe {
                close(kq);
            }
            return Err(err);
        }
        Ok(port)
    }

    fn register_user_event(&self, ident: usize) -> io::Result<()> {
        let change = Self::user_event(ident, EV_ADD | EV_ENABLE | EV_CLEAR | EV_RECEIPT, 0);
        let mut receipt = Self::empty_event();
        let result = unsafe { kevent(self.kq, &change, 1, &mut receipt, 1, ptr::null()) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Self::check_receipt(&receipt)
    }

    fn trigger_user_event(&self, ident: usize) -> io::Result<()> {
        let change = Self::user_event(ident, EV_ADD | EV_RECEIPT, NOTE_TRIGGER);
        let mut receipt = Self::empty_event();
        let result = unsafe { kevent(self.kq, &change, 1, &mut receipt, 1, ptr::null()) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Self::check_receipt(&receipt)
    }

    fn drain_completion(&self) -> Option<CompletionEnvelope> {
        self.queue
            .lock()
            .expect("kqueue completion queue poisoned")
            .pop_front()
    }

    fn drain_io_completion(&self) -> Option<Box<dyn FnOnce() + Send>> {
        self.io_completions
            .lock()
            .expect("kqueue io-completion queue poisoned")
            .pop_front()
    }

    fn allocate_io_op_id(&self) -> IoOpId {
        IoOpId(self.next_io_op_id.fetch_add(1, Ordering::Relaxed) | IO_OP_TAG)
    }

    /// Queues an already-resolved `IoPort` completion (an immediate
    /// success/failure from the optimistic first attempt, or a
    /// `cancel_io` cancellation) and wakes the pump the same way `post()`
    /// does -- these two queues are drained together, see `poll()`.
    fn queue_io_completion(&self, thunk: Box<dyn FnOnce() + Send>) -> io::Result<()> {
        self.io_completions
            .lock()
            .expect("kqueue io-completion queue poisoned")
            .push_back(thunk);
        self.trigger_user_event(QUEUE_IDENT)
    }

    /// Registers a one-shot readiness wait for `filter` (`EVFILT_READ` or
    /// `EVFILT_WRITE`) tagged with `op_id`, picked up later by `poll()`'s
    /// `resolve_io_readiness`.
    fn register_io_wait(&self, fd: RawFd, filter: i16, op_id: IoOpId) -> io::Result<()> {
        let change = kevent {
            ident: fd as _,
            filter,
            flags: (EV_ADD | EV_ONESHOT | EV_RECEIPT) as _,
            fflags: 0,
            data: 0,
            udata: op_id.0 as usize as *mut _,
        };
        let mut receipt = Self::empty_event();
        let result = unsafe { kevent(self.kq, &change, 1, &mut receipt, 1, ptr::null()) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Self::check_receipt(&receipt)
    }

    /// Sets `O_NONBLOCK` on `fd`. Necessary, not just a convenience: once
    /// a one-shot readiness wait fires, this backend performs the real
    /// syscall right there on the poll thread -- if the fd were still
    /// blocking, a spurious wakeup or a race where something else already
    /// consumed the data would make that syscall genuinely block,
    /// stalling every other operation sharing this proactor. Also what
    /// lets `connect`'s standard non-blocking-connect-then-poll-writable
    /// pattern work at all.
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

    fn user_event(ident: usize, flags: impl Into<u32>, fflags: u32) -> kevent {
        kevent {
            ident: ident as _,
            filter: EVFILT_USER,
            flags: flags.into() as _,
            fflags,
            data: 0,
            udata: ptr::null_mut(),
        }
    }

    /// Looks up and removes `op_id`'s `InFlightOp` and performs the real
    /// syscall it's been waiting to make, now that its one-shot readiness
    /// wait has fired. Returns a benign `PollEvent::Wake` for an `op_id`
    /// with no matching entry -- shouldn't happen in practice (the
    /// registration is one-shot and only ever created alongside the
    /// entry), but a stray/duplicate event is far better tolerated than
    /// panicking on it.
    fn resolve_io_readiness(&self, op_id: IoOpId) -> PollEvent {
        let entry = self
            .in_flight
            .lock()
            .expect("kqueue in-flight op table poisoned")
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

    /// Interprets a `read`/`recv`/`recvfrom`-style syscall's return value
    /// (bytes read on success, `-1` with `errno` set on failure), calling
    /// `io::Error::last_os_error()` immediately -- callers must not run
    /// any other libc call between the syscall and this, or errno could
    /// be clobbered first.
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
        // unreachable-pattern warning -- this form stays correct (if
        // redundant) even if that ever stopped being true somewhere.
        let code = err.raw_os_error();
        code == Some(libc::EAGAIN) || code == Some(libc::EWOULDBLOCK)
    }

    fn read_event(fd: RawFd, flags: impl Into<u32>, token: u64) -> kevent {
        kevent {
            ident: fd as _,
            filter: EVFILT_READ,
            flags: flags.into() as _,
            fflags: 0,
            data: 0,
            udata: token as usize as *mut _,
        }
    }

    fn empty_event() -> kevent {
        unsafe { MaybeUninit::<kevent>::zeroed().assume_init() }
    }

    fn check_receipt(event: &kevent) -> io::Result<()> {
        if event.flags & EV_ERROR != 0 && event.data != 0 {
            Err(io::Error::from_raw_os_error(event.data as i32))
        } else {
            Ok(())
        }
    }

    fn duration_to_timespec(duration: Duration) -> timespec {
        timespec {
            tv_sec: duration.as_secs() as _,
            tv_nsec: duration.subsec_nanos() as _,
        }
    }
}

impl CompletionPort for KqueuePort {
    fn post(&self, envelope: CompletionEnvelope) -> io::Result<()> {
        self.queue
            .lock()
            .expect("kqueue completion queue poisoned")
            .push_back(envelope);
        self.trigger_user_event(QUEUE_IDENT)
    }

    fn poll(&self, timeout: Option<Duration>) -> io::Result<PollEvent> {
        if let Some(envelope) = self.drain_completion() {
            return Ok(PollEvent::Completion(envelope));
        }
        if let Some(thunk) = self.drain_io_completion() {
            return Ok(PollEvent::IoCompletion(thunk));
        }

        let mut event = Self::empty_event();
        let mut timeout_storage = timeout.map(Self::duration_to_timespec);
        let timeout_ptr = timeout_storage
            .as_mut()
            .map_or(ptr::null(), |timespec| timespec as *mut _ as *const _);

        // Retries on EINTR: a stray signal delivered to this thread during
        // the blocking kevent() call (ptrace attach, SIGCHLD from an
        // unrelated part of the process, etc.) must not be allowed to
        // propagate as an error here -- see uring.rs's poll() for the
        // matching fix and how this was actually found (proactor-harness's
        // throughput bench, via an strace attach). Retrying the wait
        // itself is correct and doesn't need to resubmit anything: unlike
        // io_uring, kqueue has no separate "submission" step for this
        // call, so there's nothing that could have partially landed.
        let result = loop {
            let attempt = unsafe { kevent(self.kq, ptr::null(), 0, &mut event, 1, timeout_ptr) };
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
        if event.flags & EV_ERROR != 0 && event.data != 0 {
            return Err(io::Error::from_raw_os_error(event.data as i32));
        }

        if event.filter == EVFILT_USER {
            if event.ident == QUEUE_IDENT {
                if let Some(envelope) = self.drain_completion() {
                    return Ok(PollEvent::Completion(envelope));
                }
                if let Some(thunk) = self.drain_io_completion() {
                    return Ok(PollEvent::IoCompletion(thunk));
                }
                return Ok(PollEvent::Wake);
            }
            if event.ident == WAKE_IDENT {
                return Ok(PollEvent::Wake);
            }
        }

        if event.filter == EVFILT_READ || event.filter == EVFILT_WRITE {
            let token = event.udata as usize as u64;
            if token & IO_OP_TAG != 0 {
                return Ok(self.resolve_io_readiness(IoOpId(token)));
            }
            if event.filter == EVFILT_READ {
                return Ok(PollEvent::Readiness(ReadinessEvent { token }));
            }
        }

        Ok(PollEvent::Wake)
    }

    fn wake(&self) -> io::Result<()> {
        self.trigger_user_event(WAKE_IDENT)
    }

    fn begin_shutdown(&self) {
        let op_ids: Vec<IoOpId> = self
            .in_flight
            .lock()
            .expect("kqueue in-flight op table poisoned")
            .keys()
            .copied()
            .collect();
        for op_id in op_ids {
            // Unlike IoUringPort's cancel_io, this is synchronous and
            // immediate -- kqueue's EV_DELETE doesn't need to wait for
            // any kernel-side confirmation the way an io_uring
            // AsyncCancel's own completion does, so shutdown_complete()
            // becomes true right after this loop finishes, not after some
            // later poll() cycle.
            let _ = self.cancel_io(op_id);
        }
    }

    fn shutdown_complete(&self) -> bool {
        self.in_flight
            .lock()
            .expect("kqueue in-flight op table poisoned")
            .is_empty()
    }
}

impl ReadinessPort for KqueuePort {
    fn register_readable(&self, fd: RawFd, token: u64) -> io::Result<()> {
        let change = Self::read_event(fd, EV_ADD | EV_ENABLE | EV_CLEAR | EV_RECEIPT, token);
        let mut receipt = Self::empty_event();
        let result = unsafe { kevent(self.kq, &change, 1, &mut receipt, 1, ptr::null()) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Self::check_receipt(&receipt)
    }

    fn deregister(&self, fd: RawFd) -> io::Result<()> {
        let change = Self::read_event(fd, EV_DELETE | EV_RECEIPT, 0);
        let mut receipt = Self::empty_event();
        let result = unsafe { kevent(self.kq, &change, 1, &mut receipt, 1, ptr::null()) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Self::check_receipt(&receipt)
    }
}

impl IoPort for KqueuePort {
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
            .expect("kqueue in-flight op table poisoned")
            .insert(
                op_id,
                InFlightOp::Read {
                    fd,
                    buf,
                    offset,
                    handler,
                },
            );
        self.register_io_wait(fd, EVFILT_READ, op_id)?;
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
            .expect("kqueue in-flight op table poisoned")
            .insert(
                op_id,
                InFlightOp::Write {
                    fd,
                    buf,
                    offset,
                    handler,
                },
            );
        self.register_io_wait(fd, EVFILT_WRITE, op_id)?;
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
            .expect("kqueue in-flight op table poisoned")
            .insert(op_id, InFlightOp::Recv { fd, buf, handler });
        self.register_io_wait(fd, EVFILT_READ, op_id)?;
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
            .expect("kqueue in-flight op table poisoned")
            .insert(op_id, InFlightOp::Send { fd, buf, handler });
        self.register_io_wait(fd, EVFILT_WRITE, op_id)?;
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
            .expect("kqueue in-flight op table poisoned")
            .insert(op_id, InFlightOp::RecvFrom { fd, buf, handler });
        self.register_io_wait(fd, EVFILT_READ, op_id)?;
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
            .expect("kqueue in-flight op table poisoned")
            .insert(
                op_id,
                InFlightOp::SendTo {
                    fd,
                    buf,
                    target,
                    handler,
                },
            );
        self.register_io_wait(fd, EVFILT_WRITE, op_id)?;
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
            .expect("kqueue in-flight op table poisoned")
            .insert(op_id, InFlightOp::Accept { fd, handler });
        self.register_io_wait(fd, EVFILT_READ, op_id)?;
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
            .expect("kqueue in-flight op table poisoned")
            .insert(op_id, InFlightOp::Connect { fd, handler });
        self.register_io_wait(fd, EVFILT_WRITE, op_id)?;
        Ok(op_id)
    }

    fn cancel_io(&self, op: IoOpId) -> io::Result<()> {
        let entry = self
            .in_flight
            .lock()
            .expect("kqueue in-flight op table poisoned")
            .remove(&op);
        let Some(entry) = entry else {
            return Ok(());
        };

        let (fd, filter) = match &entry {
            InFlightOp::Read { fd, .. } | InFlightOp::Recv { fd, .. } => (*fd, EVFILT_READ),
            InFlightOp::Write { fd, .. } | InFlightOp::Send { fd, .. } => (*fd, EVFILT_WRITE),
            InFlightOp::RecvFrom { fd, .. } | InFlightOp::Accept { fd, .. } => (*fd, EVFILT_READ),
            InFlightOp::SendTo { fd, .. } | InFlightOp::Connect { fd, .. } => (*fd, EVFILT_WRITE),
        };
        // Best-effort: if the registration already fired (racing this
        // cancellation) there's nothing left to delete, which is fine --
        // the entry's already been removed above either way.
        let change = kevent {
            ident: fd as _,
            filter,
            flags: (EV_DELETE | EV_RECEIPT) as _,
            fflags: 0,
            data: 0,
            udata: ptr::null_mut(),
        };
        let mut receipt = Self::empty_event();
        unsafe {
            kevent(self.kq, &change, 1, &mut receipt, 1, ptr::null());
        }

        let cancelled_err = || io::Error::new(io::ErrorKind::Interrupted, "operation cancelled");
        let thunk: Box<dyn FnOnce() + Send> = match entry {
            InFlightOp::Read { handler, .. }
            | InFlightOp::Write { handler, .. }
            | InFlightOp::Recv { handler, .. }
            | InFlightOp::Send { handler, .. }
            | InFlightOp::RecvFrom { handler, .. }
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

impl Drop for KqueuePort {
    fn drop(&mut self) {
        unsafe {
            close(self.kq);
        }
    }
}
