//! Bounded worker offload for CPU work an app must not do on its frame
//! thread, such as decoding images.
//!
//! `offload(job)` (exported by every host) runs `job` on a small process-wide
//! worker pool and hands the result back as a completion on the host's
//! proactor, so it lands on the host's dispatch thread like any other I/O
//! result. The app reads it with [`Offloaded::try_take`] on a later frame; it
//! never waits for it. Results arrive between frames. On macOS a delivered
//! result also runs a frame, so an app with an idle frame demand takes it at
//! once; on the other hosts it sees the result on its next frame (input, a
//! deadline, a resize).
//!
//! The pool is deliberately small (two workers) to keep a phone cool. It queues
//! without limit, so the caller bounds how many jobs it has in flight; that
//! bound is also what bounds the memory held by unread results.
//!
//! Work that must not hold a pool worker, such as waiting minutes on a child
//! process, runs on a thread of its own and hands its result back through
//! `completion()` (also exported by every host): a [`Completer`] that thread
//! finishes, paired with the [`Offloaded`] the app reads, delivered the same
//! way.

use std::sync::{Arc, Mutex, PoisonError};

/// A job's result, or the panic message if the job panicked.
pub type OffloadResult<T> = Result<T, String>;

/// The handle `offload` returns.
pub struct Offloaded<T> {
    slot: Arc<Mutex<Option<OffloadResult<T>>>>,
}

impl<T> Offloaded<T> {
    /// The job's result once the host has delivered it, else `None`. Returns
    /// the result once; later calls return `None`.
    pub fn try_take(&mut self) -> Option<OffloadResult<T>> {
        self.slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// An already finished job, for hosts with no proactor (which run the job
    /// inline).
    #[cfg_attr(
        any(
            target_os = "macos",
            target_os = "ios",
            target_os = "linux",
            target_os = "android",
            target_os = "windows"
        ),
        allow(dead_code)
    )]
    pub(crate) fn ready(result: T) -> Self {
        Self {
            slot: Arc::new(Mutex::new(Some(Ok(result)))),
        }
    }
}

/// The finishing half of `completion()`: whoever holds it delivers the result
/// to the paired [`Offloaded`], from any thread. Dropping it unfinished
/// delivers an error, so the reader is never left waiting.
pub struct Completer<T: Send + 'static> {
    deliver: Option<Box<dyn FnOnce(OffloadResult<T>) + Send>>,
}

impl<T: Send + 'static> Completer<T> {
    pub fn complete(mut self, value: T) {
        if let Some(deliver) = self.deliver.take() {
            deliver(Ok(value));
        }
    }
}

impl<T: Send + 'static> Drop for Completer<T> {
    fn drop(&mut self) {
        if let Some(deliver) = self.deliver.take() {
            deliver(Err("the work ended without a result".to_string()));
        }
    }
}

/// A [`Completer`] and its [`Offloaded`]. `post` hands the host a closure that
/// stores the result; the host runs it where results are delivered.
#[cfg_attr(target_os = "netbsd", allow(dead_code))]
pub(crate) fn completion_via<T: Send + 'static>(
    post: impl FnOnce(Box<dyn FnOnce() + Send>) + Send + 'static,
) -> (Completer<T>, Offloaded<T>) {
    let slot = Arc::new(Mutex::new(None));
    let delivered = Arc::clone(&slot);
    let deliver = Box::new(move |result: OffloadResult<T>| {
        post(Box::new(move || {
            *delivered.lock().unwrap_or_else(PoisonError::into_inner) = Some(result);
        }));
    });
    (
        Completer {
            deliver: Some(deliver),
        },
        Offloaded { slot },
    )
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android",
    target_os = "windows"
))]
// macOS delivers through `offload_through_with_wake` instead.
#[cfg_attr(target_os = "macos", allow(unused_imports))]
pub(crate) use pool::offload_through;
#[cfg(target_os = "macos")]
pub(crate) use pool::offload_through_with_wake;

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android",
    target_os = "windows"
))]
mod pool {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};
    use std::thread;

    use loadngo_proactor::{CompletionPort, ProactorHandle};

    use super::{OffloadResult, Offloaded};

    /// Worker threads in the pool. Two keep a decode burst off the frame thread
    /// without lighting up every core of a phone.
    pub(super) const WORKERS: usize = 2;

    type Job = Box<dyn FnOnce() + Send>;

    struct Pool {
        sender: Sender<Job>,
    }

    fn pool() -> Option<&'static Pool> {
        static POOL: OnceLock<Option<Pool>> = OnceLock::new();
        POOL.get_or_init(start).as_ref()
    }

    fn start() -> Option<Pool> {
        let (sender, receiver) = mpsc::channel::<Job>();
        let receiver = Arc::new(Mutex::new(receiver));
        let mut started = 0;
        for index in 0..WORKERS {
            let receiver = Arc::clone(&receiver);
            let spawned = thread::Builder::new()
                .name(format!("loadngo-offload-{index}"))
                .spawn(move || run_worker(&receiver));
            if spawned.is_ok() {
                started += 1;
            }
        }
        (started > 0).then_some(Pool { sender })
    }

    fn run_worker(receiver: &Mutex<Receiver<Job>>) {
        loop {
            // Held only while waiting for a job, never while running one.
            let job = match receiver.lock() {
                Ok(receiver) => receiver.recv(),
                Err(_) => return,
            };
            let Ok(job) = job else {
                return;
            };
            job();
        }
    }

    /// Runs `job` on the pool and delivers its result through `handle`.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub(crate) fn offload_through<P, T>(
        handle: ProactorHandle<P>,
        job: impl FnOnce() -> T + Send + 'static,
    ) -> Offloaded<T>
    where
        P: CompletionPort,
        T: Send + 'static,
    {
        offload_through_with_wake(handle, job, || {})
    }

    /// As [`offload_through`], then calls `wake` on the worker thread once
    /// the result is queued. A host whose thread waits somewhere other than
    /// the completion port (macOS waits in AppKit's event queue) uses it to
    /// get the result dispatched and an idle frame run.
    pub(crate) fn offload_through_with_wake<P, T>(
        handle: ProactorHandle<P>,
        job: impl FnOnce() -> T + Send + 'static,
        wake: impl FnOnce() + Send + 'static,
    ) -> Offloaded<T>
    where
        P: CompletionPort,
        T: Send + 'static,
    {
        let slot = Arc::new(Mutex::new(None));
        let delivered = Arc::clone(&slot);
        let work: Job = Box::new(move || {
            let result: OffloadResult<T> = catch_unwind(AssertUnwindSafe(job)).map_err(|payload| {
                let message = payload
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".to_string());
                format!("offloaded job panicked: {message}")
            });
            // Fails only once the host proactor has stopped, when nobody is
            // left to read the result.
            let queued = handle.enqueue_work(move |_| {
                *delivered.lock().unwrap_or_else(PoisonError::into_inner) = Some(result);
            });
            if queued.is_ok() {
                wake();
            }
        });
        let sent = pool().is_some_and(|pool| pool.sender.send(work).is_ok());
        if !sent {
            *slot.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(Err("no offload worker could be started".to_string()));
        }
        Offloaded { slot }
    }

    #[cfg(test)]
    mod tests {
        use super::offload_through;
        use loadngo_proactor::{ChannelPort, Proactor};
        use std::time::{Duration, Instant};

        fn run_until<T>(
            proactor: &Proactor<ChannelPort>,
            job: &mut super::Offloaded<T>,
        ) -> super::OffloadResult<T> {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(result) = job.try_take() {
                    return result;
                }
                assert!(Instant::now() < deadline, "offloaded job never delivered");
                proactor.run_once().expect("proactor poll failed");
            }
        }

        #[test]
        fn a_result_arrives_only_through_the_proactor() {
            let proactor = Proactor::new(ChannelPort::new());
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let mut job = offload_through(proactor.handle(), move || {
                started_tx.send(()).unwrap();
                6 * 7
            });
            started_rx.recv().unwrap();
            // The worker has run, but nothing has dispatched its completion.
            std::thread::sleep(Duration::from_millis(50));
            assert!(job.try_take().is_none());
            assert_eq!(run_until(&proactor, &mut job), Ok(42));
            assert!(job.try_take().is_none(), "a result is taken once");
        }

        #[test]
        fn a_panicking_job_reports_its_message_and_the_pool_survives() {
            let proactor = Proactor::new(ChannelPort::new());
            let mut failing = offload_through(proactor.handle(), || -> u32 {
                panic!("bad frame");
            });
            let error = run_until(&proactor, &mut failing).unwrap_err();
            assert!(error.contains("bad frame"), "{error}");
            for value in 0..2 * super::WORKERS as u32 {
                let mut job = offload_through(proactor.handle(), move || value);
                assert_eq!(run_until(&proactor, &mut job), Ok(value));
            }
        }
    }
}

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux", target_os = "windows")
))]
mod completion_tests {
    use super::completion_via;
    use loadngo_proactor::{ChannelPort, Proactor};
    use std::time::{Duration, Instant};

    fn pair(proactor: &Proactor<ChannelPort>) -> (super::Completer<u32>, super::Offloaded<u32>) {
        let handle = proactor.handle();
        completion_via(move |store| {
            let _ = handle.enqueue_work(move |_| store());
        })
    }

    fn wait(
        proactor: &Proactor<ChannelPort>,
        reader: &mut super::Offloaded<u32>,
    ) -> Result<u32, String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(result) = reader.try_take() {
                return result;
            }
            assert!(Instant::now() < deadline, "no result delivered");
            proactor.run_once().expect("proactor poll failed");
        }
    }

    #[test]
    fn a_result_completed_on_another_thread_arrives_through_the_proactor() {
        let proactor = Proactor::new(ChannelPort::new());
        let (completer, mut reader) = pair(&proactor);
        std::thread::spawn(move || completer.complete(7))
            .join()
            .unwrap();
        assert!(reader.try_take().is_none(), "not until the proactor runs");
        assert_eq!(wait(&proactor, &mut reader), Ok(7));
    }

    #[test]
    fn dropping_the_completer_delivers_an_error() {
        let proactor = Proactor::new(ChannelPort::new());
        let (completer, mut reader) = pair(&proactor);
        drop(completer);
        assert!(wait(&proactor, &mut reader).is_err());
    }
}
