mod channel;
mod deferred;
mod error;
mod io_port;
#[cfg(windows)]
mod iocp;
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
mod kqueue;
#[cfg(target_os = "linux")]
mod uring;

use deferred::DeferredQueue;
use std::io;
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
#[cfg(unix)]
use std::{collections::HashMap, os::fd::RawFd};

pub use channel::ChannelPort;
pub use error::ProactorError;
pub use io_port::{
    AcceptCompletionHandler, AcceptResult, AcceptTransfer, IoBuf, IoCompletionHandler, IoOpId,
    IoPort, IoResult, IoTransfer, RawFdCompat, UnitCompletionHandler,
};
#[cfg(windows)]
pub use iocp::IocpPort;
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
pub use kqueue::KqueuePort;
#[cfg(target_os = "linux")]
pub use uring::IoUringPort;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    Exit,
    Job,
    Net,
    Io,
    Timer,
    User(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Completion {
    pub kind: CompletionKind,
    pub bytes_transferred: u32,
}

pub trait CompletionHandler: Send + 'static {
    fn run(self: Box<Self>, completion: Completion);
}

impl<F> CompletionHandler for F
where
    F: FnOnce(Completion) + Send + 'static,
{
    fn run(self: Box<Self>, completion: Completion) {
        (self)(completion);
    }
}

pub struct CompletionEnvelope {
    completion: Completion,
    handler: Box<dyn CompletionHandler>,
}

impl CompletionEnvelope {
    pub fn new(
        kind: CompletionKind,
        bytes_transferred: u32,
        handler: impl CompletionHandler,
    ) -> Self {
        Self {
            completion: Completion {
                kind,
                bytes_transferred,
            },
            handler: Box::new(handler),
        }
    }

    pub fn completion(&self) -> Completion {
        self.completion
    }

    fn dispatch(self) {
        self.handler.run(self.completion);
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadinessEvent {
    pub token: u64,
}

#[cfg(unix)]
pub trait ReadinessHandler: Send + 'static {
    fn on_ready(&mut self, readiness: ReadinessEvent);
}

#[cfg(unix)]
impl<F> ReadinessHandler for F
where
    F: FnMut(ReadinessEvent) + Send + 'static,
{
    fn on_ready(&mut self, readiness: ReadinessEvent) {
        (self)(readiness);
    }
}

pub enum PollEvent {
    Completion(CompletionEnvelope),
    /// A completed `IoPort` operation, already fully resolved by the
    /// backend into a ready-to-run thunk (it already looked up the
    /// buffer/handler for whatever `IoOpId` the kernel's completion named
    /// and built the real `IoResult`/`AcceptResult`/etc.) -- `Proactor`
    /// just runs it, the same way it runs a `CompletionEnvelope`.
    IoCompletion(Box<dyn FnOnce() + Send>),
    #[cfg(unix)]
    Readiness(ReadinessEvent),
    Wake,
    Timeout,
}

pub trait CompletionPort: Send + Sync + 'static {
    fn post(&self, envelope: CompletionEnvelope) -> io::Result<()>;
    fn poll(&self, timeout: Option<Duration>) -> io::Result<PollEvent>;
    fn wake(&self) -> io::Result<()>;

    /// Called once by `Proactor::run_until_stopped`/`run_ready`'s owning
    /// loop after `running` becomes false, before the loop actually
    /// exits. Default is a no-op; `IoPort` implementations override it to
    /// request cancellation of every still-outstanding operation, since
    /// their buffers can't be safely reclaimed until the kernel confirms
    /// it's truly done with them. Not meant to be called directly.
    fn begin_shutdown(&self) {}

    /// Polled once per loop iteration during the drain phase that follows
    /// `begin_shutdown`; the loop keeps calling `poll()` (so cancellation
    /// completions can actually land and get dispatched) until this
    /// returns `true`. Default is always `true` -- nothing to drain for a
    /// backend with no in-flight buffer-owning operations.
    fn shutdown_complete(&self) -> bool {
        true
    }
}

#[cfg(unix)]
pub trait ReadinessPort: CompletionPort {
    fn register_readable(&self, fd: RawFd, token: u64) -> io::Result<()>;
    fn deregister(&self, fd: RawFd) -> io::Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunReport {
    pub dispatched_completions: usize,
    pub dispatched_deferred: usize,
    pub woke: bool,
    pub stopped: bool,
}

impl RunReport {
    fn idle(stopped: bool) -> Self {
        Self {
            dispatched_completions: 0,
            dispatched_deferred: 0,
            woke: false,
            stopped,
        }
    }
}

#[cfg(unix)]
type ReadinessHandlers = HashMap<u64, Arc<Mutex<Box<dyn ReadinessHandler>>>>;

struct Shared<P> {
    port: P,
    deferred: Mutex<DeferredQueue>,
    #[cfg(unix)]
    readiness: Mutex<ReadinessHandlers>,
    next_sequence: AtomicU64,
    running: AtomicBool,
}

impl<P> Shared<P> {
    fn allocate_sequence(&self) -> u64 {
        self.next_sequence.fetch_add(1, Ordering::Relaxed)
    }
}

pub struct Proactor<P> {
    shared: Arc<Shared<P>>,
}

pub struct ProactorHandle<P> {
    shared: Arc<Shared<P>>,
}

impl<P> Clone for ProactorHandle<P> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<P> Proactor<P>
where
    P: CompletionPort,
{
    pub fn new(port: P) -> Self {
        Self {
            shared: Arc::new(Shared {
                port,
                deferred: Mutex::new(DeferredQueue::default()),
                #[cfg(unix)]
                readiness: Mutex::new(HashMap::new()),
                next_sequence: AtomicU64::new(1),
                running: AtomicBool::new(true),
            }),
        }
    }

    pub fn handle(&self) -> ProactorHandle<P> {
        ProactorHandle {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Polls once and dispatches whatever comes back, with no `running`
    /// check at all. Factored out because `run_until_stopped`'s drain
    /// phase (below) must keep actually polling even though `running` is
    /// already `false` by the time it runs -- reusing `run_once` there
    /// directly was a real bug: `run_once` early-returns the instant
    /// `running` is `false`, without ever calling `poll()`, which turned
    /// the drain loop into an unbounded, 100%-CPU busy-spin that never
    /// gave `IoPort::begin_shutdown`'s cancellation a chance to actually
    /// land or its completion to arrive. Confirmed as a real (not just
    /// theoretical) hang: it ran for 35+ minutes on `dolores` before
    /// being killed, and independently timed out a live CI job the same
    /// way (`cargo test --workspace` there runs this same test suite).
    fn poll_and_dispatch_once(&self, timeout: Option<Duration>) -> io::Result<RunReport> {
        let mut report = RunReport::idle(false);
        match self.shared.port.poll(timeout)? {
            PollEvent::Completion(envelope) => {
                envelope.dispatch();
                report.dispatched_completions += 1;
            }
            PollEvent::IoCompletion(thunk) => {
                thunk();
                report.dispatched_completions += 1;
            }
            #[cfg(unix)]
            PollEvent::Readiness(readiness) => {
                self.dispatch_readiness(readiness);
                report.woke = true;
            }
            PollEvent::Wake => {
                report.woke = true;
            }
            PollEvent::Timeout => {}
        }
        Ok(report)
    }

    pub fn run_once(&self) -> io::Result<RunReport> {
        let mut report = RunReport::idle(!self.shared.running.load(Ordering::Acquire));
        report.dispatched_deferred += self.dispatch_ready_deferred(Instant::now())?;

        if !self.shared.running.load(Ordering::Acquire) {
            report.stopped = true;
            return Ok(report);
        }

        let timeout = {
            let deferred = self
                .shared
                .deferred
                .lock()
                .expect("deferred queue poisoned");
            deferred.time_until_next_deadline(Instant::now())
        };

        let poll_report = self.poll_and_dispatch_once(timeout)?;
        report.dispatched_completions += poll_report.dispatched_completions;
        report.woke = poll_report.woke;

        report.dispatched_deferred += self.dispatch_ready_deferred(Instant::now())?;
        report.stopped = !self.shared.running.load(Ordering::Acquire);
        Ok(report)
    }

    /// Runs until `stop()` is called, then -- before returning -- drains
    /// any still-outstanding `IoPort` operations so it's always safe for
    /// the caller to drop `self`/the backing port immediately afterward.
    /// A backend with no such operations (or one that doesn't implement
    /// `IoPort` at all) no-ops this via `CompletionPort`'s default
    /// `begin_shutdown`/`shutdown_complete`, so this is a safe drop-in
    /// replacement for what used to be the entire method body.
    pub fn run_until_stopped(&self) -> io::Result<()> {
        while self.shared.running.load(Ordering::Acquire) {
            let report = self.run_once()?;
            if report.stopped {
                break;
            }
        }

        self.shared.port.begin_shutdown();
        while !self.shared.port.shutdown_complete() {
            // A short, bounded timeout rather than blocking indefinitely:
            // shutdown_complete() must be re-checked periodically even if
            // a given backend's cancellation doesn't itself produce a
            // wake-worthy event, so this can't just wait forever on poll()
            // the way run_once()'s normal-operation path safely can.
            self.poll_and_dispatch_once(Some(Duration::from_millis(50)))?;
        }
        Ok(())
    }

    pub fn run_ready(&self) -> io::Result<RunReport> {
        let mut report = RunReport::idle(!self.shared.running.load(Ordering::Acquire));
        report.dispatched_deferred += self.dispatch_ready_deferred(Instant::now())?;

        if !self.shared.running.load(Ordering::Acquire) {
            report.stopped = true;
            return Ok(report);
        }

        let poll_report = self.poll_and_dispatch_once(Some(Duration::ZERO))?;
        report.dispatched_completions += poll_report.dispatched_completions;
        report.woke = poll_report.woke;

        report.dispatched_deferred += self.dispatch_ready_deferred(Instant::now())?;
        report.stopped = !self.shared.running.load(Ordering::Acquire);
        Ok(report)
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        let deferred = self
            .shared
            .deferred
            .lock()
            .expect("deferred queue poisoned");
        deferred.next_deadline()
    }

    fn dispatch_ready_deferred(&self, now: Instant) -> io::Result<usize> {
        let ready = {
            let mut deferred = self
                .shared
                .deferred
                .lock()
                .expect("deferred queue poisoned");
            deferred.take_ready(now)
        };

        let count = ready.len();
        for envelope in ready {
            envelope.dispatch();
        }
        Ok(count)
    }

    #[cfg(unix)]
    fn dispatch_readiness(&self, readiness: ReadinessEvent) {
        let handler = {
            let readiness_handlers = self
                .shared
                .readiness
                .lock()
                .expect("readiness handler registry poisoned");
            readiness_handlers.get(&readiness.token).cloned()
        };

        if let Some(handler) = handler {
            handler
                .lock()
                .expect("readiness handler poisoned")
                .on_ready(readiness);
        }
    }
}

impl<P> ProactorHandle<P>
where
    P: CompletionPort,
{
    pub fn enqueue(
        &self,
        kind: CompletionKind,
        bytes_transferred: u32,
        handler: impl CompletionHandler,
    ) -> io::Result<()> {
        self.shared
            .port
            .post(CompletionEnvelope::new(kind, bytes_transferred, handler))
    }

    pub fn enqueue_work(&self, handler: impl CompletionHandler) -> io::Result<()> {
        self.enqueue(CompletionKind::Job, 0, handler)
    }

    pub fn defer_until(
        &self,
        when: Instant,
        kind: CompletionKind,
        bytes_transferred: u32,
        handler: impl CompletionHandler,
    ) -> io::Result<()> {
        {
            let mut deferred = self
                .shared
                .deferred
                .lock()
                .expect("deferred queue poisoned");
            deferred.push(
                self.shared.allocate_sequence(),
                when,
                CompletionEnvelope::new(kind, bytes_transferred, handler),
            );
        }
        self.shared.port.wake()
    }

    pub fn defer_for(
        &self,
        delay: Duration,
        kind: CompletionKind,
        bytes_transferred: u32,
        handler: impl CompletionHandler,
    ) -> io::Result<()> {
        self.defer_until(Instant::now() + delay, kind, bytes_transferred, handler)
    }

    pub fn stop(&self) -> io::Result<()> {
        self.shared.running.store(false, Ordering::Release);
        self.shared.port.wake()
    }

    pub fn wake(&self) -> io::Result<()> {
        self.shared.port.wake()
    }

    pub fn is_running(&self) -> bool {
        self.shared.running.load(Ordering::Acquire)
    }
}

#[cfg(unix)]
impl<P> ProactorHandle<P>
where
    P: ReadinessPort,
{
    pub fn register_readable(
        &self,
        fd: RawFd,
        token: u64,
        handler: impl ReadinessHandler,
    ) -> io::Result<()> {
        let shared_handler = Arc::new(Mutex::new(Box::new(handler) as Box<dyn ReadinessHandler>));
        {
            let mut readiness_handlers = self
                .shared
                .readiness
                .lock()
                .expect("readiness handler registry poisoned");
            if readiness_handlers.contains_key(&token) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("readiness token {token} already registered"),
                ));
            }
            readiness_handlers.insert(token, Arc::clone(&shared_handler));
        }

        if let Err(err) = self.shared.port.register_readable(fd, token) {
            self.shared
                .readiness
                .lock()
                .expect("readiness handler registry poisoned")
                .remove(&token);
            return Err(err);
        }

        Ok(())
    }

    pub fn deregister_readable(&self, fd: RawFd, token: u64) -> io::Result<()> {
        self.shared
            .readiness
            .lock()
            .expect("readiness handler registry poisoned")
            .remove(&token);
        self.shared.port.deregister(fd)
    }
}

impl<P> ProactorHandle<P>
where
    P: IoPort,
{
    pub fn read(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        offset: u64,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        self.shared.port.read(fd, buf, offset, handler)
    }

    pub fn write(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        offset: u64,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        self.shared.port.write(fd, buf, offset, handler)
    }

    pub fn recv(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        self.shared.port.recv(fd, buf, handler)
    }

    pub fn recv_from(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        self.shared.port.recv_from(fd, buf, handler)
    }

    pub fn send(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        self.shared.port.send(fd, buf, handler)
    }

    pub fn send_to(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        target: SocketAddr,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        self.shared.port.send_to(fd, buf, target, handler)
    }

    pub fn accept(
        &self,
        fd: RawFdCompat,
        handler: impl AcceptCompletionHandler,
    ) -> io::Result<IoOpId> {
        self.shared.port.accept(fd, handler)
    }

    pub fn connect(
        &self,
        fd: RawFdCompat,
        target: SocketAddr,
        handler: impl UnitCompletionHandler,
    ) -> io::Result<IoOpId> {
        self.shared.port.connect(fd, target, handler)
    }

    pub fn cancel_io(&self, op: IoOpId) -> io::Result<()> {
        self.shared.port.cancel_io(op)
    }
}
