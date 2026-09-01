//! Shared host-proactor ownership pattern.
//!
//! Every desktop host that owns a `loadngo_proactor::Proactor` (macOS today,
//! Linux as of this module) needs the same three things: somewhere to hold
//! the `Proactor`/`ProactorHandle` pair, a `Waker` that pokes the completion
//! port so a blocked native event pump or `poll()` re-checks the runtime
//! future, and a drain loop that keeps dispatching ready work until a poll
//! comes back with nothing left to do. This was previously hand-rolled once
//! per host (see `macos.rs`'s old `MacProactor`/`RuntimeWakeSignal`); this
//! module is the single copy per
//! `docs/PROACTOR_ENGINE_ADOPTION.md`'s "portable host-driver seam" (Phase
//! 1).

use std::sync::Arc;
use std::task::{Wake, Waker};

use loadngo_proactor::{CompletionPort, Proactor, ProactorHandle, RunReport};

pub struct HostProactor<P: CompletionPort> {
    pub proactor: Proactor<P>,
    pub handle: ProactorHandle<P>,
}

impl<P: CompletionPort> HostProactor<P> {
    pub fn new(port: P) -> Self {
        let proactor = Proactor::new(port);
        let handle = proactor.handle();
        Self { proactor, handle }
    }

    /// Dispatches everything currently ready (completions, due deferred
    /// work) and keeps going until a poll comes back with no activity at
    /// all. Safe to call on every native-event-pump wakeup: a poll with
    /// nothing to do returns immediately (`run_ready` never blocks).
    pub fn drain_ready(&self) {
        loop {
            let report = self
                .proactor
                .run_ready()
                .expect("failed to drain host proactor");
            if !report_has_activity(report) {
                break;
            }
        }
    }

    /// A `Waker` for this proactor: waking it just pokes the completion
    /// port (`ProactorHandle::wake`), which is enough to break a blocked
    /// native event pump or `poll()` out of its wait so the runtime future
    /// gets re-polled. Used today by macOS's `poll_entry_future` (driving
    /// the top-level entry future with a raw `Context::from_waker`); Linux
    /// wakes its pending futures directly instead (see
    /// `linux.rs::HostSharedState::next_frame_wakers`), so it doesn't call
    /// this yet -- kept here rather than moved into `macos.rs` because iOS
    /// (the next platform migration, also `KqueuePort`-backed) is expected
    /// to need the same raw-executor pattern macOS uses.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn waker(&self) -> Waker {
        waker_for(self.handle.clone())
    }
}

fn report_has_activity(report: RunReport) -> bool {
    report.dispatched_completions > 0
        || report.dispatched_deferred > 0
        || report.woke
        || report.stopped
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct ProactorWakeSignal<P: CompletionPort> {
    handle: ProactorHandle<P>,
}

impl<P: CompletionPort> Wake for ProactorWakeSignal<P> {
    fn wake(self: Arc<Self>) {
        let _ = self.handle.wake();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let _ = self.handle.wake();
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn waker_for<P: CompletionPort>(handle: ProactorHandle<P>) -> Waker {
    Waker::from(Arc::new(ProactorWakeSignal { handle }))
}
