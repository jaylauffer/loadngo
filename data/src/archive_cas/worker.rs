//! A bounded CPU worker lane for Archive CAS operations.
//!
//! The caller applies thermal admission before [`ArchiveCasWorkerPool::try_submit`]
//! and drives the supplied proactor until a task result is ready. Completed task
//! results count against the same outstanding limit until taken or dropped. A
//! job's own share is given up as its result becomes visible, so a caller that
//! has taken a result can submit again at once; the wakeup posted to the
//! proactor holds no capacity.

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
/// share of the pool's outstanding-work reservation. A job that has not
/// finished keeps its own share until it has, so dropping the handle early
/// does not let more work in than the pool allows.
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
            {
                // The result becomes visible and the job gives up its share of the limit under
                // one lock, so a caller that takes the result finds the capacity free. Until
                // 2026-11-24 the share was held until the wakeup below had been dispatched and
                // this thread had dropped its own reference after posting it. A caller whose
                // result was ready when it first looked never ran its proactor, and on Linux
                // posting the wakeup runs the caller before this thread drops: the caller's
                // next submission was refused as saturated (three repository tests on a Pi 5,
                // one on a Pi 4; none on macOS).
                let mut slot = result.lock().unwrap_or_else(PoisonError::into_inner);
                *slot = Some(outcome);
                drop(permit);
            }
            // Wakes the caller's event loop. It carries no capacity: at most one is pending for
            // each finished job, and every wait on a result dispatches them.
            let _ = handle.enqueue_work(|_| {});
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

#[cfg(test)]
mod tests {
    use super::*;
    use loadngo_proactor::new_platform_proactor;
    use std::time::{Duration, Instant};

    /// Waits for a result without ever running the proactor: the caller that finds its result
    /// ready when it first looks.
    fn take_without_dispatching<T>(task: &mut WorkerTask<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(result) = task.try_take() {
                return result.expect("the job panicked");
            }
            assert!(Instant::now() < deadline, "the job never finished");
            std::thread::yield_now();
        }
    }

    #[test]
    fn a_taken_result_frees_its_capacity_before_its_wakeup_is_dispatched() {
        let proactor = new_platform_proactor().expect("proactor");
        let handle = proactor.handle();
        // The repository's blocking-I/O lane: one worker, one queued, two outstanding at most.
        let pool = ArchiveCasWorkerPool::new(1, 1).expect("pool");
        for round in 0..64 {
            let mut task = pool
                .try_submit(&handle, move || round)
                .unwrap_or_else(|error| panic!("submission {round} was refused: {error}"));
            assert_eq!(take_without_dispatching(&mut task), round);
            assert!(
                !pool.is_saturated(),
                "round {round} left the lane saturated"
            );
        }
        drop(proactor);
    }

    /// Waits until `count` tasks hold a share of the limit (a task holds one from its submission
    /// until its job has stored the result and the result has been taken or dropped).
    fn settle(pool: &ArchiveCasWorkerPool, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while pool.outstanding.load(Ordering::Acquire) != count {
            assert!(
                Instant::now() < deadline,
                "the lane never settled at {count}"
            );
            std::thread::yield_now();
        }
    }

    /// A job that says when the worker has taken it up (the one-job queue is free again then).
    fn announced<T: Send + 'static>(
        value: T,
    ) -> (impl FnOnce() -> T + Send + 'static, mpsc::Receiver<()>) {
        let (started, seen) = mpsc::channel();
        (
            move || {
                started.send(()).ok();
                value
            },
            seen,
        )
    }

    #[test]
    fn work_still_running_or_untaken_counts_against_the_limit() {
        let proactor = new_platform_proactor().expect("proactor");
        let handle = proactor.handle();
        let pool = ArchiveCasWorkerPool::new(1, 1).expect("pool");
        // Two results left untaken fill the lane; a third submission is refused.
        let (job, taken_up) = announced(1);
        let mut first = pool.try_submit(&handle, job).expect("first");
        taken_up.recv().expect("the first job ran");
        let (job, taken_up) = announced(2);
        let mut second = pool.try_submit(&handle, job).expect("second");
        assert!(pool.is_saturated());
        assert_eq!(
            pool.try_submit(&handle, || 3).err(),
            Some(WorkerSubmitError::Saturated)
        );
        // Taking one makes room for exactly one more.
        assert_eq!(take_without_dispatching(&mut first), 1);
        taken_up.recv().expect("the second job ran");
        let mut third = pool.try_submit(&handle, || 3).expect("third");
        assert!(pool.is_saturated());
        assert_eq!(take_without_dispatching(&mut second), 2);
        assert_eq!(take_without_dispatching(&mut third), 3);
        settle(&pool, 0);
        // A handle dropped while its job still runs keeps the share until the job ends.
        let (started, running) = mpsc::channel::<()>();
        let (release, held) = mpsc::channel::<()>();
        let abandoned = pool
            .try_submit(&handle, move || {
                started.send(()).ok();
                held.recv().ok()
            })
            .expect("a job that waits");
        running.recv().expect("the job started");
        drop(abandoned);
        assert_eq!(pool.outstanding.load(Ordering::Acquire), 1);
        let mut queued = pool.try_submit(&handle, || ()).expect("one behind it");
        assert_eq!(
            pool.try_submit(&handle, || ()).err(),
            Some(WorkerSubmitError::Saturated)
        );
        release.send(()).expect("the job is waiting");
        take_without_dispatching(&mut queued);
        settle(&pool, 0);
        drop(proactor);
    }
}
