mod channel;
mod deferred;
#[cfg(target_os = "linux")]
mod uring;
mod error;
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

use deferred::DeferredQueue;
use std::io;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
#[cfg(unix)]
use std::{collections::HashMap, os::fd::RawFd};

pub use channel::ChannelPort;
#[cfg(target_os = "linux")]
pub use uring::IoUringPort;
pub use error::ProactorError;
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
    #[cfg(unix)]
    Readiness(ReadinessEvent),
    Wake,
    Timeout,
}

pub trait CompletionPort: Send + Sync + 'static {
    fn post(&self, envelope: CompletionEnvelope) -> io::Result<()>;
    fn poll(&self, timeout: Option<Duration>) -> io::Result<PollEvent>;
    fn wake(&self) -> io::Result<()>;
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

struct Shared<P> {
    port: P,
    deferred: Mutex<DeferredQueue>,
    #[cfg(unix)]
    readiness: Mutex<HashMap<u64, Arc<Mutex<Box<dyn ReadinessHandler>>>>>,
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

        match self.shared.port.poll(timeout)? {
            PollEvent::Completion(envelope) => {
                envelope.dispatch();
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

        report.dispatched_deferred += self.dispatch_ready_deferred(Instant::now())?;
        report.stopped = !self.shared.running.load(Ordering::Acquire);
        Ok(report)
    }

    pub fn run_until_stopped(&self) -> io::Result<()> {
        while self.shared.running.load(Ordering::Acquire) {
            let report = self.run_once()?;
            if report.stopped {
                break;
            }
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

        match self.shared.port.poll(Some(Duration::ZERO))? {
            PollEvent::Completion(envelope) => {
                envelope.dispatch();
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
