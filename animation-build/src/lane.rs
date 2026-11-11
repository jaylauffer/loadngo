//! A bounded CPU worker lane with thermal admission, delivering results through the
//! proactor.
//!
//! - **Bounded.** A fixed number of worker threads and at most that many jobs in flight;
//!   the caller keeps whatever it has not submitted. Nothing queues without limit.
//! - **Thermal admission.** Before each submission the lane consults the platform's
//!   thermal provider through a [`ThermalGovernor`], sampling no more often than the
//!   provider's pressure allows (`ThermalPressure::sample_interval`: 5 s nominal, 1 s
//!   critical). It admits all workers at nominal (or unavailable) pressure, half at fair,
//!   one at serious and none at critical, when the caller waits on a proactor deadline
//!   for the next sample rather than spinning.
//! - **Results as completions.** A worker posts its result to the proactor
//!   (`ProactorHandle::enqueue`), so the dispatching thread wakes for job results and I/O
//!   alike, blocked in the port rather than polling. A panicking job reports its message.
//!
//! Before calling a run thermally safe, measure it on the target hardware; the lane
//! bounds the work but does not prove anything about heat.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use loadngo_proactor::{CompletionKind, CompletionPort, ProactorHandle};
use loadngo_thermal::{GovernorConfig, ThermalGovernor, ThermalPressure, ThermalProvider};

type Job<R> = Box<dyn FnOnce() -> R + Send>;

/// What a job returned, or its panic message.
pub type JobResult<R> = Result<R, String>;

/// A bounded lane of worker threads.
pub struct Lane<R: Send + 'static, P: CompletionPort> {
    jobs: Option<SyncSender<(usize, Job<R>)>>,
    workers: Vec<JoinHandle<()>>,
    results: Receiver<(usize, JobResult<R>)>,
    in_flight: usize,
    provider: Box<dyn ThermalProvider>,
    governor: ThermalGovernor,
    next_sample: Instant,
    _port: std::marker::PhantomData<P>,
    /// How many times admission held a submission back, for the run's report.
    pub held_back: u64,
    pub pressure_seen: Vec<ThermalPressure>,
}

fn panic_message(p: Box<dyn Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "a job panicked".to_string()
    }
}

impl<R: Send + 'static, P: CompletionPort> Lane<R, P> {
    /// Starts `workers` threads (at least one) whose results are posted to `proactor`.
    ///
    /// # Panics
    /// When a thread cannot be spawned.
    pub fn new(
        workers: usize,
        proactor: &ProactorHandle<P>,
        provider: Box<dyn ThermalProvider>,
    ) -> Self {
        let workers = workers.max(1);
        let (job_tx, job_rx) = mpsc::sync_channel::<(usize, Job<R>)>(workers);
        let job_rx = std::sync::Arc::new(std::sync::Mutex::new(job_rx));
        let (res_tx, res_rx) = mpsc::channel::<(usize, JobResult<R>)>();
        let mut handles = Vec::with_capacity(workers);
        for i in 0..workers {
            let rx = std::sync::Arc::clone(&job_rx);
            let tx: Sender<(usize, JobResult<R>)> = res_tx.clone();
            let handle = proactor.clone();
            handles.push(
                thread::Builder::new()
                    .name(format!("anim-build-{i}"))
                    .spawn(move || loop {
                        let next = rx.lock().map(|r| r.recv());
                        let Ok(Ok((id, job))) = next else { break };
                        let r = catch_unwind(AssertUnwindSafe(job)).map_err(panic_message);
                        let tx = tx.clone();
                        // Deliver on the proactor's dispatch thread. If the proactor has
                        // stopped, the result is dropped with the run.
                        let _ = handle.enqueue(CompletionKind::Job, 0, move |_| {
                            let _ = tx.send((id, r));
                        });
                    })
                    .expect("spawn a worker thread"),
            );
        }
        Self {
            jobs: Some(job_tx),
            workers: handles,
            results: res_rx,
            in_flight: 0,
            provider,
            governor: ThermalGovernor::new(GovernorConfig::default()),
            next_sample: Instant::now(),
            _port: std::marker::PhantomData,
            held_back: 0,
            pressure_seen: Vec::new(),
        }
    }

    /// Workers in the lane.
    #[must_use]
    pub fn workers(&self) -> usize {
        self.workers.len()
    }

    /// Jobs submitted and not yet taken back with [`Lane::take`].
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.in_flight
    }

    /// The current thermal pressure, sampling the provider if its interval has passed.
    pub fn pressure(&mut self) -> ThermalPressure {
        let now = Instant::now();
        if now >= self.next_sample {
            let obs = self.provider.observe();
            self.governor.observe(&obs, now);
            let p = self.governor.snapshot().pressure;
            if self.pressure_seen.last() != Some(&p) {
                self.pressure_seen.push(p);
            }
            self.next_sample = now + p.sample_interval();
        }
        self.governor.snapshot().pressure
    }

    /// How many jobs may be in flight at `pressure`.
    #[must_use]
    pub fn admitted(&self, pressure: ThermalPressure) -> usize {
        let w = self.workers.len();
        match pressure {
            ThermalPressure::Unavailable | ThermalPressure::Nominal => w,
            ThermalPressure::Fair => w.div_ceil(2),
            ThermalPressure::Serious => 1,
            ThermalPressure::Critical => 0,
        }
    }

    /// Whether a job may be submitted now.
    pub fn can_submit(&mut self) -> bool {
        let p = self.pressure();
        let ok = self.in_flight < self.admitted(p);
        if !ok && self.in_flight < self.workers.len() {
            self.held_back += 1;
        }
        ok
    }

    /// How long until admission may change by itself (the next thermal sample).
    #[must_use]
    pub fn until_next_sample(&self) -> Duration {
        self.next_sample.saturating_duration_since(Instant::now())
    }

    /// Submits `job` tagged `id`. Call only after [`Lane::can_submit`] said yes.
    ///
    /// # Panics
    /// When the lane has been shut down.
    pub fn submit(&mut self, id: usize, job: impl FnOnce() -> R + Send + 'static) {
        self.in_flight += 1;
        self.jobs
            .as_ref()
            .expect("the lane is running")
            .send((id, Box::new(job)))
            .expect("a worker is alive");
    }

    /// A result the proactor has delivered, if any.
    pub fn take(&mut self) -> Option<(usize, JobResult<R>)> {
        let r = self.results.try_recv().ok();
        if r.is_some() {
            self.in_flight -= 1;
        }
        r
    }
}

impl<R: Send + 'static, P: CompletionPort> Drop for Lane<R, P> {
    fn drop(&mut self) {
        self.jobs = None;
        for h in self.workers.drain(..) {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loadngo_thermal::{FakeProvider, ThermalPressure};

    #[test]
    fn results_arrive_through_the_proactor_and_admission_follows_pressure() {
        let proactor = loadngo_proactor::new_platform_proactor().unwrap();
        let mut lane: Lane<u64, _> = Lane::new(
            4,
            &proactor.handle(),
            Box::new(FakeProvider::new(vec![ThermalPressure::Nominal])),
        );
        assert_eq!(lane.admitted(ThermalPressure::Nominal), 4);
        assert_eq!(lane.admitted(ThermalPressure::Fair), 2);
        assert_eq!(lane.admitted(ThermalPressure::Serious), 1);
        assert_eq!(lane.admitted(ThermalPressure::Critical), 0);
        let mut sum = 0;
        let mut next = 0u64;
        let mut done = 0;
        while done < 20 {
            while next < 20 && lane.can_submit() {
                let n = next;
                lane.submit(n as usize, move || n * n);
                next += 1;
            }
            assert!(lane.in_flight() <= 4);
            proactor.run_once().unwrap();
            while let Some((_, r)) = lane.take() {
                sum += r.unwrap();
                done += 1;
            }
        }
        assert_eq!(sum, (0..20u64).map(|n| n * n).sum::<u64>());
    }

    #[test]
    fn a_panicking_job_reports_its_message() {
        let proactor = loadngo_proactor::new_platform_proactor().unwrap();
        let mut lane: Lane<u8, _> = Lane::new(
            1,
            &proactor.handle(),
            Box::new(FakeProvider::new(vec![ThermalPressure::Nominal])),
        );
        lane.submit(7, || panic!("broken input"));
        loop {
            proactor.run_once().unwrap();
            if let Some((id, r)) = lane.take() {
                assert_eq!(id, 7);
                assert!(r.unwrap_err().contains("broken input"));
                break;
            }
        }
    }

    #[test]
    fn critical_pressure_admits_nothing() {
        let proactor = loadngo_proactor::new_platform_proactor().unwrap();
        let mut lane: Lane<u8, _> = Lane::new(
            2,
            &proactor.handle(),
            Box::new(FakeProvider::new(vec![ThermalPressure::Critical])),
        );
        assert!(!lane.can_submit());
        assert!(lane.until_next_sample() <= Duration::from_secs(1));
    }
}
