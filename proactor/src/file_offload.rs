//! Worker threads for blocking file I/O on the readiness-based backends.
//!
//! kqueue and epoll report readiness, and a regular file or block device is always
//! "ready": `pread`/`pwrite` on one never returns `EAGAIN`, it simply blocks for as long
//! as the storage takes. `KqueuePort` and `EpollPort` used to perform that syscall inside
//! `IoPort::read`/`write` on the submitting thread, so a batch of file reads ran one
//! after another no matter how many were submitted. `io_uring` and IOCP perform file
//! I/O asynchronously in the kernel and never had this limit.
//!
//! This pool gives the two readiness backends the same property. A file op is handed to
//! a worker, which performs the positioned syscall and queues the completion on the
//! port's existing completion queue, waking `poll` exactly as an immediately-resolved
//! op does. Sockets, pipes and character devices keep the readiness path.
//!
//! Workers start on the first file op, so a proactor that never touches a file -- a
//! host's frame timer, a network loop -- never creates a thread.

use std::collections::VecDeque;
use std::io;
use std::num::NonZeroUsize;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

/// A resolved completion, run on the thread that polls the port.
pub(crate) type Thunk = Box<dyn FnOnce() + Send>;

/// A port's queue of resolved completions, shared with the workers.
pub(crate) type CompletionQueue = Arc<Mutex<VecDeque<Thunk>>>;

/// Performs one blocking syscall on a worker and returns the completion to deliver.
pub(crate) type FileJob = Box<dyn FnOnce() -> Thunk + Send>;

/// Worker bounds. Storage rather than CPU limits this work, so the pool may run more
/// threads than there are cores; 16 matches the widest batch the heaviest current
/// consumer submits at once (a model step's top-16 routed-expert loads).
const MIN_WORKERS: usize = 4;
const MAX_WORKERS: usize = 16;

/// Number of workers a pool starts.
pub(crate) fn worker_count() -> usize {
    thread::available_parallelism()
        .map_or(MIN_WORKERS, NonZeroUsize::get)
        .clamp(MIN_WORKERS, MAX_WORKERS)
}

/// Whether `fd` is a regular file or block device: the kinds whose reads and writes
/// block without ever reporting readiness. Anything `fstat` cannot describe keeps the
/// readiness path, where its syscall reports the real error.
pub(crate) fn blocks_without_readiness(fd: RawFd) -> bool {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return false;
    }
    let kind = unsafe { stat.assume_init() }.st_mode & libc::S_IFMT;
    kind == libc::S_IFREG || kind == libc::S_IFBLK
}

pub(crate) struct FileOffload {
    completions: CompletionQueue,
    notify: Arc<dyn Fn() + Send + Sync>,
    /// Jobs submitted but whose completion is not yet queued. A port is not shut down
    /// while any remain, because their buffers are still with a worker.
    pending: Arc<AtomicUsize>,
    pool: Mutex<Option<Pool>>,
}

struct Pool {
    sender: Sender<FileJob>,
    workers: Vec<JoinHandle<()>>,
}

impl FileOffload {
    /// `notify` must wake the port's `poll` the way queueing an immediate completion
    /// does. It is called from worker threads.
    pub(crate) fn new(completions: CompletionQueue, notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            completions,
            notify,
            pending: Arc::new(AtomicUsize::new(0)),
            pool: Mutex::new(None),
        }
    }

    /// Hands `job` to a worker, starting the pool on first use.
    pub(crate) fn submit(&self, job: FileJob) -> io::Result<()> {
        let mut pool = self.pool.lock().unwrap_or_else(PoisonError::into_inner);
        if pool.is_none() {
            *pool = Some(self.start()?);
        }
        let Some(running) = pool.as_ref() else {
            return Err(io::Error::other("file I/O workers are unavailable"));
        };
        self.pending.fetch_add(1, Ordering::SeqCst);
        if running.sender.send(job).is_err() {
            self.pending.fetch_sub(1, Ordering::SeqCst);
            return Err(io::Error::other("file I/O workers have stopped"));
        }
        Ok(())
    }

    /// Jobs submitted whose completion has not been queued yet.
    pub(crate) fn pending(&self) -> usize {
        self.pending.load(Ordering::SeqCst)
    }

    /// Stops accepting jobs, lets the workers finish every job already queued, and joins
    /// them. A port calls this in `Drop` before closing the handle `notify` uses.
    pub(crate) fn shutdown(&self) {
        let pool = self
            .pool
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(Pool { sender, workers }) = pool {
            drop(sender);
            for worker in workers {
                let _ = worker.join();
            }
        }
    }

    fn start(&self) -> io::Result<Pool> {
        let (sender, receiver) = mpsc::channel::<FileJob>();
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::new();
        for index in 0..worker_count() {
            let receiver = Arc::clone(&receiver);
            let completions = Arc::clone(&self.completions);
            let notify = Arc::clone(&self.notify);
            let pending = Arc::clone(&self.pending);
            let spawned = thread::Builder::new()
                .name(format!("loadngo-file-io-{index}"))
                .spawn(move || run_worker(&receiver, &completions, notify.as_ref(), &pending));
            match spawned {
                Ok(handle) => workers.push(handle),
                // No worker at all is an error; fewer than planned still works.
                Err(error) if workers.is_empty() => return Err(error),
                Err(_) => break,
            }
        }
        Ok(Pool { sender, workers })
    }
}

impl Drop for FileOffload {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_worker(
    receiver: &Mutex<Receiver<FileJob>>,
    completions: &Mutex<VecDeque<Thunk>>,
    notify: &(dyn Fn() + Send + Sync),
    pending: &AtomicUsize,
) {
    loop {
        // The receiver lock is held only while waiting for a job, never while running
        // one, so every idle worker can pick up the next job.
        let job = match receiver.lock() {
            Ok(receiver) => receiver.recv(),
            Err(_) => return,
        };
        // A closed channel is shutdown, and only arrives once the queue is empty.
        let Ok(job) = job else {
            return;
        };
        let completion = job();
        completions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(completion);
        pending.fetch_sub(1, Ordering::SeqCst);
        notify();
    }
}

#[cfg(test)]
mod tests {
    use super::{worker_count, CompletionQueue, FileOffload};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    fn offload() -> (FileOffload, CompletionQueue) {
        let completions: CompletionQueue = Arc::new(Mutex::new(VecDeque::new()));
        let offload = FileOffload::new(Arc::clone(&completions), Arc::new(|| {}));
        (offload, completions)
    }

    fn wait_until_idle(offload: &FileOffload) {
        let start = Instant::now();
        while offload.pending() > 0 {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "offloaded jobs never finished"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn a_batch_of_jobs_runs_concurrently_not_one_after_another() {
        // Each job waits, up to five seconds, until every job in the batch is running at
        // the same moment. Workers that ran jobs serially could never satisfy that, so
        // every job would time out instead of counting itself as overlapped.
        let (offload, completions) = offload();
        let batch = worker_count().min(4);
        let gate = Arc::new((Mutex::new(0_usize), Condvar::new()));
        let overlapped = Arc::new(AtomicUsize::new(0));

        for _ in 0..batch {
            let gate = Arc::clone(&gate);
            let overlapped = Arc::clone(&overlapped);
            offload
                .submit(Box::new(move || {
                    let (running, all_running) = &*gate;
                    let mut count = running.lock().unwrap();
                    *count += 1;
                    all_running.notify_all();
                    let (_count, wait) = all_running
                        .wait_timeout_while(count, Duration::from_secs(5), |count| *count < batch)
                        .unwrap();
                    if !wait.timed_out() {
                        overlapped.fetch_add(1, Ordering::SeqCst);
                    }
                    Box::new(|| {})
                }))
                .unwrap();
        }

        wait_until_idle(&offload);
        assert_eq!(
            overlapped.load(Ordering::SeqCst),
            batch,
            "the batch did not run concurrently"
        );
        assert_eq!(completions.lock().unwrap().len(), batch);
    }

    #[test]
    fn no_worker_thread_starts_before_the_first_job() {
        let (offload, _) = offload();
        assert!(offload.pool.lock().unwrap().is_none());
        offload.submit(Box::new(|| Box::new(|| {}))).unwrap();
        assert!(offload.pool.lock().unwrap().is_some());
    }

    #[test]
    fn shutdown_finishes_every_queued_job_before_returning() {
        let (offload, completions) = offload();
        for _ in 0..64 {
            offload
                .submit(Box::new(|| {
                    thread::sleep(Duration::from_millis(1));
                    Box::new(|| {})
                }))
                .unwrap();
        }
        offload.shutdown();
        assert_eq!(offload.pending(), 0);
        assert_eq!(completions.lock().unwrap().len(), 64);
    }
}
