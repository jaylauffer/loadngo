mod channel;
mod deferred;
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

pub use channel::ChannelPort;
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

pub enum PollEvent {
    Completion(CompletionEnvelope),
    Wake,
    Timeout,
}

pub trait CompletionPort: Send + Sync + 'static {
    fn post(&self, envelope: CompletionEnvelope) -> io::Result<()>;
    fn poll(&self, timeout: Option<Duration>) -> io::Result<PollEvent>;
    fn wake(&self) -> io::Result<()>;
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
