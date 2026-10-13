//! Closing the audio output when nothing has played for a while, shared by
//! the iOS and native desktop backends.
//!
//! An open output costs CPU whether or not anything is audible: the device
//! calls the render callback every few milliseconds to ask for more samples.
//! Measured on the Mac mini (2026-10-13, `sng-mahjong` on its title menu with
//! nothing playing): 0.3% CPU and four extra threads with the output open,
//! 0.0% without. For a game that idles between taps that is most of what it
//! costs. Android's AAudio mixer already stops after three silent seconds;
//! this gives the other owned backends the same rule.
//!
//! Two pieces:
//!
//! - [`IdlePolicy`] decides, on the game thread, when the output should close
//!   and when the game must come back to close it. Pure logic, so it is
//!   tested without a device.
//! - [`OutputSwitch`] carries the decision to loadngo's offload pool, which
//!   does the actual open, start, stop or close, because those block: on the
//!   Mac mini a reopen took 8-11 ms and a close about 40 ms, either of which
//!   would cost the game a frame. Requests are serialized, so two pool
//!   workers never switch the device at once, and needs no running host
//!   (`audio_harness` and the tests have none).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long the output stays open with nothing playing. Matches Android's
/// `IDLE_STOP`: long enough that a player tapping steadily never waits on a
/// reopen, short enough that a still screen stops costing anything soon.
pub(crate) const IDLE_STOP: Duration = Duration::from_secs(3);

/// The shortest wait [`IdlePolicy::demand`] asks for, so a deadline that is
/// already due still yields a real (if immediate) frame request.
const MIN_DEMAND: Duration = Duration::from_millis(1);

/// When to close the output and when the game must come back to do it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IdlePolicy {
    /// Whether the output is open, or has been asked to open.
    open: bool,
    /// When the mixer was first seen silent since the output last played.
    quiet_since: Option<Instant>,
}

impl IdlePolicy {
    pub(crate) const fn new(open: bool) -> Self {
        Self {
            open,
            quiet_since: None,
        }
    }

    #[cfg(test)]
    const fn is_open(&self) -> bool {
        self.open
    }

    /// Something is about to play. True when the output was closed and must
    /// be asked to open.
    pub(crate) fn wake(&mut self) -> bool {
        self.quiet_since = None;
        let was_closed = !self.open;
        self.open = true;
        was_closed
    }

    /// The game thread saw the mixer `quiet` (nothing to play) or not at
    /// `now`. True when the output has been silent for [`IDLE_STOP`] and must
    /// be asked to close.
    pub(crate) fn observe(&mut self, quiet: bool, now: Instant) -> bool {
        if !self.open || !quiet {
            self.quiet_since = None;
            return false;
        }
        let since = *self.quiet_since.get_or_insert(now);
        if now.saturating_duration_since(since) < IDLE_STOP {
            return false;
        }
        self.open = false;
        self.quiet_since = None;
        true
    }

    /// The open failed: treat the output as closed, so the next play tries
    /// again rather than waiting on a device that never started.
    pub(crate) fn open_failed(&mut self) {
        self.open = false;
        self.quiet_since = None;
    }

    /// How long the game may idle before it must call `update` again so the
    /// output can close: the time left on the silence deadline, or the whole
    /// deadline if the silence has not been noticed yet. `None` while the
    /// output is closed or something plays; playing voices and tracks ask for
    /// their own frames.
    pub(crate) fn demand(&self, quiet: bool, now: Instant) -> Option<Duration> {
        if !self.open || !quiet {
            return None;
        }
        let remaining = self.quiet_since.map_or(IDLE_STOP, |since| {
            IDLE_STOP.saturating_sub(now.saturating_duration_since(since))
        });
        Some(remaining.max(MIN_DEMAND))
    }
}

/// The sooner of two frame demands, either of which may be absent.
pub(crate) fn sooner(a: Option<Duration>, b: Option<Duration>) -> Option<Duration> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Brings the output to the most recently requested state on the offload
/// pool.
///
/// Requests are latest-wins and serialized: at most one pool job switches
/// the device at a time, and it keeps going until the state it applied is
/// still the one wanted, so a close followed at once by an open leaves the
/// output open and nothing stale is replayed.
#[derive(Clone)]
pub(crate) struct OutputSwitch {
    shared: Arc<SwitchShared>,
}

struct SwitchShared {
    desired: AtomicBool,
    /// A job is queued or running and will look at `desired` again.
    busy: AtomicBool,
    apply: Mutex<Box<dyn FnMut(bool) + Send>>,
}

impl OutputSwitch {
    /// `apply(open)` runs on the pool for each change and must tolerate
    /// being asked for the state it is already in.
    pub(crate) fn new(open: bool, apply: impl FnMut(bool) + Send + 'static) -> Self {
        Self {
            shared: Arc::new(SwitchShared {
                desired: AtomicBool::new(open),
                busy: AtomicBool::new(false),
                apply: Mutex::new(Box::new(apply)),
            }),
        }
    }

    pub(crate) fn request(&self, open: bool) {
        self.shared.desired.store(open, Ordering::Release);
        if self.shared.busy.swap(true, Ordering::AcqRel) {
            // The job in flight reads `desired` again before it finishes.
            return;
        }
        let shared = Arc::clone(&self.shared);
        crate::offload::run_detached(move || loop {
            let wanted = shared.desired.load(Ordering::Acquire);
            {
                let mut apply = shared.apply.lock().unwrap_or_else(PoisonError::into_inner);
                (*apply)(wanted);
            }
            shared.busy.store(false, Ordering::Release);
            // A request after `wanted` was read either found `busy` set and
            // left it to this job, or cleared it and started its own job;
            // claiming `busy` again tells the two apart.
            if shared.desired.load(Ordering::Acquire) == wanted
                || shared.busy.swap(true, Ordering::AcqRel)
            {
                break;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn at(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    #[test]
    fn the_output_closes_after_three_quiet_seconds_and_not_before() {
        let start = Instant::now();
        let mut policy = IdlePolicy::new(true);
        assert!(!policy.observe(true, start));
        assert!(!policy.observe(true, at(start, 2_999)));
        assert!(policy.is_open());
        assert!(policy.observe(true, at(start, 3_000)), "due at 3 s");
        assert!(!policy.is_open());
        assert!(
            !policy.observe(true, at(start, 9_000)),
            "a closed output is not closed again"
        );
    }

    #[test]
    fn sound_restarts_the_silence_clock() {
        let start = Instant::now();
        let mut policy = IdlePolicy::new(true);
        policy.observe(true, start);
        policy.observe(false, at(start, 2_500));
        assert!(
            !policy.observe(true, at(start, 3_500)),
            "quiet again from 3.5 s"
        );
        assert!(!policy.observe(true, at(start, 6_400)));
        assert!(policy.observe(true, at(start, 6_500)));
    }

    #[test]
    fn a_play_reopens_a_closed_output_once() {
        let start = Instant::now();
        let mut policy = IdlePolicy::new(true);
        assert!(!policy.wake(), "already open");
        policy.observe(true, start);
        assert!(policy.observe(true, at(start, 3_000)));
        assert!(policy.wake(), "closed, so it must be asked to open");
        assert!(!policy.wake(), "asked once is enough");
        assert!(policy.is_open());
    }

    #[test]
    fn a_play_during_the_quiet_wait_cancels_the_close() {
        let start = Instant::now();
        let mut policy = IdlePolicy::new(true);
        policy.observe(true, start);
        assert!(!policy.wake());
        assert!(
            !policy.observe(true, at(start, 3_000)),
            "the silence clock restarted at the play"
        );
    }

    #[test]
    fn demand_wakes_the_game_once_at_the_deadline() {
        let start = Instant::now();
        let mut policy = IdlePolicy::new(true);
        assert_eq!(
            policy.demand(true, start),
            Some(IDLE_STOP),
            "not yet noticed"
        );
        policy.observe(true, start);
        assert_eq!(
            policy.demand(true, at(start, 1_000)),
            Some(Duration::from_millis(2_000))
        );
        assert_eq!(policy.demand(true, at(start, 5_000)), Some(MIN_DEMAND));
        assert_eq!(policy.demand(false, start), None, "playing asks for itself");
        policy.observe(true, at(start, 3_000));
        assert_eq!(policy.demand(true, at(start, 4_000)), None, "closed");
    }

    #[test]
    fn a_failed_open_is_retried_on_the_next_play() {
        let mut policy = IdlePolicy::new(false);
        assert!(policy.wake());
        policy.open_failed();
        assert!(policy.wake());
    }

    #[test]
    fn sooner_picks_the_earlier_demand() {
        let short = Some(Duration::from_millis(100));
        let long = Some(Duration::from_secs(3));
        assert_eq!(sooner(short, long), short);
        assert_eq!(sooner(long, short), short);
        assert_eq!(sooner(None, long), long);
        assert_eq!(sooner(long, None), long);
        assert_eq!(sooner(None, None), None);
    }

    #[test]
    fn the_switch_applies_the_latest_request() {
        let (applied, seen) = channel();
        let switch = OutputSwitch::new(true, move |open| {
            let _ = applied.send(open);
        });
        switch.request(false);
        assert_eq!(
            seen.recv_timeout(Duration::from_secs(5)),
            Ok(false),
            "a close reaches the pool"
        );
        switch.request(true);
        assert_eq!(seen.recv_timeout(Duration::from_secs(5)), Ok(true));
        // A burst of requests: whatever is applied last is the latest one.
        for _ in 0..50 {
            switch.request(false);
            switch.request(true);
        }
        // The pool is shared with every other test, so allow for queueing:
        // wait for the first switch, then until none follows for a while.
        let mut last = seen.recv_timeout(Duration::from_secs(5)).ok();
        while let Ok(open) = seen.recv_timeout(Duration::from_secs(2)) {
            last = Some(open);
        }
        assert_eq!(last, Some(true));
    }

    #[test]
    fn the_switch_never_applies_twice_at_once() {
        use std::sync::atomic::AtomicUsize;
        let inside = Arc::new(AtomicUsize::new(0));
        let overlap = Arc::new(AtomicBool::new(false));
        let (done, finished) = channel();
        let (inside_job, overlap_job) = (Arc::clone(&inside), Arc::clone(&overlap));
        let switch = OutputSwitch::new(true, move |open| {
            if inside_job.fetch_add(1, Ordering::SeqCst) != 0 {
                overlap_job.store(true, Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_millis(2));
            inside_job.fetch_sub(1, Ordering::SeqCst);
            let _ = done.send(open);
        });
        for index in 0..40 {
            switch.request(index % 2 == 0);
        }
        while finished.recv_timeout(Duration::from_millis(500)).is_ok() {}
        assert!(!overlap.load(Ordering::SeqCst), "two switches overlapped");
    }
}
