//! Bounded waits for the proactor tests.
//!
//! Every wait in these tests is for something that arrives in microseconds
//! to milliseconds when the backend works. A backend defect turns an
//! unbounded wait into a hang that libtest only reports ("has been running
//! for over 60 seconds") and never ends: a hosted CI job then runs until its
//! own timeout. espeak-ng-rs's Windows jobs ran six hours each on
//! 2026-10-08 that way, on a read whose IOCP completion never came. Waits
//! here go through these helpers instead, so a defect fails its test within
//! [`LIMIT`] and names what it was waiting for.

// Each test file uses a different subset.
#![allow(dead_code)]

use loadngo_proactor::{CompletionPort, Proactor, RunReport};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// The longest any one wait may take before the test fails. It only decides
/// how quickly a hang is reported, so it is generous for loaded runners.
pub const LIMIT: Duration = Duration::from_secs(10);

/// One proactor turn, failing the test if the turn has neither dispatched
/// anything nor been woken or stopped within [`LIMIT`].
pub fn run_once<P: CompletionPort>(proactor: &Proactor<P>, what: &str) -> RunReport {
    let deadline = Instant::now() + LIMIT;
    loop {
        let report = proactor.run_once_until(deadline).unwrap();
        let progressed = report.dispatched_completions > 0
            || report.dispatched_deferred > 0
            || report.woke
            || report.stopped;
        if progressed {
            return report;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: nothing arrived within {LIMIT:?}"
        );
    }
}

/// Runs proactor turns until `count` completions have been dispatched,
/// failing the test if they have not all arrived within [`LIMIT`].
pub fn run_until_dispatched<P: CompletionPort>(proactor: &Proactor<P>, count: usize, what: &str) {
    let deadline = Instant::now() + LIMIT;
    let mut dispatched = 0;
    while dispatched < count {
        assert!(
            Instant::now() < deadline,
            "{what}: {dispatched} of {count} completions within {LIMIT:?}"
        );
        dispatched += proactor
            .run_once_until(deadline)
            .unwrap()
            .dispatched_completions;
    }
}

/// A thread whose result is waited for with a bound, unlike
/// `JoinHandle::join`.
pub struct Worker<T> {
    result: mpsc::Receiver<T>,
    thread: JoinHandle<()>,
}

pub fn spawn<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> Worker<T> {
    let (tx, result) = mpsc::sync_channel(1);
    let thread = thread::spawn(move || {
        let _ = tx.send(body());
    });
    Worker { result, thread }
}

impl<T> Worker<T> {
    /// The thread's result, failing the test if it has not returned within
    /// [`LIMIT`]. A panic on the thread is re-raised here. A hung thread is
    /// left behind; the test binary's exit ends it.
    pub fn join(self, what: &str) -> T {
        match self.result.recv_timeout(LIMIT) {
            Ok(value) => {
                self.thread.join().unwrap();
                value
            }
            Err(RecvTimeoutError::Timeout) => {
                panic!("{what}: still running after {LIMIT:?}")
            }
            Err(RecvTimeoutError::Disconnected) => match self.thread.join() {
                Err(panic) => std::panic::resume_unwind(panic),
                Ok(()) => unreachable!("the worker sends its result before it returns"),
            },
        }
    }
}
