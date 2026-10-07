//! Bounded worker offload for CPU work an app must not do on its frame
//! thread, such as decoding images.
//!
//! `offload(job)` (exported by every host) runs `job` on a small process-wide
//! worker pool and hands the result back as a completion on the host's
//! proactor, so it lands on the host's dispatch thread like any other I/O
//! result. The app reads it with [`Offloaded::try_take`] on a later frame; it
//! never waits for it. Results arrive between frames, so an app with an idle
//! frame demand sees one on its next frame (input, a deadline, a resize).
//!
//! The pool is deliberately small (two workers) to keep a phone cool. It queues
//! without limit, so the caller bounds how many jobs it has in flight; that
//! bound is also what bounds the memory held by unread results.

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

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android",
    target_os = "windows"
))]
pub(crate) use pool::offload_through;

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
    pub(crate) fn offload_through<P, T>(
        handle: ProactorHandle<P>,
        job: impl FnOnce() -> T + Send + 'static,
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
            let _ = handle.enqueue_work(move |_| {
                *delivered.lock().unwrap_or_else(PoisonError::into_inner) = Some(result);
            });
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
