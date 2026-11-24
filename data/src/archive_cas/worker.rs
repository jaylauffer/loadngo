//! A bounded CPU worker lane for Archive CAS operations.
//!
//! The caller applies thermal admission before [`ArchiveCasWorkerPool::try_submit`]
//! and drives the supplied proactor until a task result is ready. Completed task
//! results count against the same outstanding limit until taken or dropped.

use loadngo_proactor::{CompletionPort, ProactorHandle};
use std::any::Any;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

type Job = Box<dyn FnOnce() + Send + 'static>;

/// Failure to queue a worker job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerSubmitError {
    /// Running, queued, and completed-but-untaken tasks have reached the limit.
    Saturated,
    /// The worker pool is stopping or has stopped.
    Stopped,
}

impl fmt::Display for WorkerSubmitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Saturated => formatter.write_str("Archive CAS worker lane is saturated"),
            Self::Stopped => formatter.write_str("Archive CAS worker lane has stopped"),
        }
    }
}

impl std::error::Error for WorkerSubmitError {}

/// A worker job panicked before producing its result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerPanic(pub String);

impl fmt::Display for WorkerPanic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Archive CAS worker panicked: {}", self.0)
    }
}

impl std::error::Error for WorkerPanic {}

/// A completed worker result. Taking or dropping this handle releases its
/// share of the pool's outstanding-work reservation. Capacity is released only
/// after the result is taken or dropped and its wakeup completion runs.
pub struct WorkerTask<T> {
    result: Arc<Mutex<Option<Result<T, WorkerPanic>>>>,
    permit: Option<Arc<Permit>>,
}

impl<T> WorkerTask<T> {
    /// Takes the result once it has completed, or returns `None` while it runs.
    pub fn try_take(&mut self) -> Option<Result<T, WorkerPanic>> {
        let result = self
            .result
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if result.is_some() {
            self.permit.take();
        }
        result
    }
}

struct Permit {
    outstanding: Arc<AtomicUsize>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.outstanding.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A fixed-size worker pool with a bounded queue and bounded total outstanding
/// work. Results post a completion to the caller's proactor to wake its event
/// loop; callers retrieve values through [`WorkerTask::try_take`].
pub struct ArchiveCasWorkerPool {
    sender: Option<SyncSender<Job>>,
    workers: Vec<JoinHandle<()>>,
    outstanding: Arc<AtomicUsize>,
    max_outstanding: usize,
}

impl ArchiveCasWorkerPool {
    /// Creates `worker_count` threads and a queue of `queue_capacity` jobs.
    /// Total running, queued, and completed-but-untaken tasks are bounded by
    /// their sum.
    pub fn new(worker_count: usize, queue_capacity: usize) -> std::io::Result<Self> {
        if worker_count == 0 || queue_capacity == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Archive CAS worker count and queue capacity must be nonzero",
            ));
        }
        let max_outstanding = worker_count.checked_add(queue_capacity).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Archive CAS worker capacity overflows usize",
            )
        })?;
        let (sender, receiver) = mpsc::sync_channel::<Job>(queue_capacity);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::with_capacity(worker_count);
        for index in 0..worker_count {
            let receiver = Arc::clone(&receiver);
            let worker = thread::Builder::new()
                .name(format!("loadngo-archive-cas-{index}"))
                .spawn(move || loop {
                    let job = {
                        let receiver = receiver.lock().unwrap_or_else(PoisonError::into_inner);
                        receiver.recv()
                    };
                    match job {
                        Ok(job) => job(),
                        Err(_) => break,
                    }
                });
            match worker {
                Ok(worker) => workers.push(worker),
                Err(error) => {
                    drop(sender);
                    for worker in workers {
                        let _ = worker.join();
                    }
                    return Err(error);
                }
            }
        }
        Ok(Self {
            sender: Some(sender),
            workers,
            outstanding: Arc::new(AtomicUsize::new(0)),
            max_outstanding,
        })
    }

    /// Returns whether all running, queued, or completed-but-untaken slots are reserved.
    ///
    /// Synchronous adapters can use this to keep servicing their proactor until
    /// completion callbacks release capacity before moving an `FnOnce` job into
    /// [`try_submit`](Self::try_submit).
    pub fn is_saturated(&self) -> bool {
        self.outstanding.load(Ordering::Acquire) >= self.max_outstanding
    }

    /// Submits CPU work without blocking. The caller should apply its thermal
    /// policy before submission and retry `Saturated` after completions free
    /// capacity. A finished task wakes the supplied proactor.
    pub fn try_submit<P, T, F>(
        &self,
        handle: &ProactorHandle<P>,
        work: F,
    ) -> Result<WorkerTask<T>, WorkerSubmitError>
    where
        P: CompletionPort + 'static,
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let mut count = self.outstanding.load(Ordering::Acquire);
        loop {
            if count >= self.max_outstanding {
                return Err(WorkerSubmitError::Saturated);
            }
            match self.outstanding.compare_exchange_weak(
                count,
                count + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => count = actual,
            }
        }

        let permit = Arc::new(Permit {
            outstanding: Arc::clone(&self.outstanding),
        });
        let result = Arc::new(Mutex::new(None));
        let task = WorkerTask {
            result: Arc::clone(&result),
            permit: Some(Arc::clone(&permit)),
        };
        let handle = handle.clone();
        let job: Job = Box::new(move || {
            let outcome = catch_unwind(AssertUnwindSafe(work))
                .map_err(|panic| WorkerPanic(panic_message(panic)));
            *result.lock().unwrap_or_else(PoisonError::into_inner) = Some(outcome);
            let wakeup_permit = Arc::clone(&permit);
            let _ = handle.enqueue_work(move |_| drop(wakeup_permit));
            drop(permit);
        });
        let Some(sender) = &self.sender else {
            drop(task);
            return Err(WorkerSubmitError::Stopped);
        };
        match sender.try_send(job) {
            Ok(()) => Ok(task),
            Err(TrySendError::Full(job)) => {
                drop(job);
                drop(task);
                Err(WorkerSubmitError::Saturated)
            }
            Err(TrySendError::Disconnected(job)) => {
                drop(job);
                drop(task);
                Err(WorkerSubmitError::Stopped)
            }
        }
    }

    /// Stops accepting work, drains the bounded queue, and joins the workers.
    /// This may wait for already-running jobs to finish.
    pub fn shutdown(&mut self) {
        self.sender.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

impl Drop for ArchiveCasWorkerPool {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn panic_message(panic: Box<dyn Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = panic.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else {
        "non-string panic payload".to_string()
    }
}
