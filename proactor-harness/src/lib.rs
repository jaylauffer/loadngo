//! Shared plumbing for `proactor-harness`'s benches and stress binaries.
//! Not part of `loadngo-proactor` itself -- deliberately a separate crate
//! (see `README.md`) so heavy dev-only deps (criterion) and load-generation
//! code never touch the library crate real consumers depend on.
//!
//! Picks the same backend `loadngo-proactor` itself would pick for the
//! current platform, so every bench/stress tool here automatically targets
//! whichever `CompletionPort` impl is actually live on the machine it runs
//! on -- `IoUringPort` on Linux (`dolores`), `KqueuePort` on macOS/iOS/BSD,
//! `EpollPort` on Android, `IocpPort` on Windows (this last one is
//! unverified by compiling, same caveat as the rest of this workspace's
//! Windows-only code -- no Windows dev machine currently available, see
//! `sng-roguelite/docs/BUILD_RELEASE_PIPELINE.md`'s iOS/Windows notes for
//! the analogous situation elsewhere in this codebase).
//!
//! Android's `EpollPort` is real, cross-compiled, and on-device-tested
//! (see `docs/PROACTOR_ENGINE_ADOPTION.md`), but this harness's own
//! benches (criterion) and the `stress` binary are dev-only tooling this
//! workspace doesn't currently build for Android at all -- `PlatformPort`
//! resolves for Android for API parity with every other platform, not
//! because anything here has actually been run through a bench on-device
//! yet.

use loadngo_proactor::{CompletionKind, Proactor, ProactorHandle};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
pub type PlatformPort = loadngo_proactor::IoUringPort;
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
pub type PlatformPort = loadngo_proactor::KqueuePort;
#[cfg(target_os = "android")]
pub type PlatformPort = loadngo_proactor::EpollPort;
#[cfg(windows)]
pub type PlatformPort = loadngo_proactor::IocpPort;

/// Constructs the same backend `loadngo-proactor` would pick for this
/// platform.
pub fn new_platform_proactor() -> io::Result<Proactor<PlatformPort>> {
    #[cfg(target_os = "linux")]
    {
        Ok(Proactor::new(PlatformPort::new()?))
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        Ok(Proactor::new(PlatformPort::new()?))
    }
    #[cfg(target_os = "android")]
    {
        Ok(Proactor::new(PlatformPort::new()?))
    }
    #[cfg(windows)]
    {
        Ok(Proactor::new(PlatformPort::new()?))
    }
}

/// Runs `proactor.run_until_stopped()` on a dedicated thread and returns a
/// handle plus the join handle, for benches/stress tools that just need a
/// live event loop pumping in the background. Callers are responsible for
/// eventually calling `handle.stop()` and joining.
pub fn spawn_pump(
    proactor: Proactor<PlatformPort>,
) -> (ProactorHandle<PlatformPort>, std::thread::JoinHandle<()>) {
    let handle = proactor.handle();
    let join = std::thread::spawn(move || {
        proactor.run_until_stopped().expect("proactor pump failed");
    });
    (handle, join)
}

/// Blocks the calling thread until `count` completions of `kind` have been
/// dispatched through `handle`'s proactor, or `timeout` elapses (in which
/// case it panics -- a benchmark/stress run that doesn't converge within a
/// generous timeout is a bug worth failing loudly on, not a slow pass).
/// Returns the wall-clock `Duration` the wait actually took.
pub fn await_completions(counter: &AtomicU64, count: u64, timeout: Duration) -> Duration {
    let start = Instant::now();
    while counter.load(Ordering::Acquire) < count {
        if start.elapsed() > timeout {
            panic!(
                "timed out after {timeout:?} waiting for {count} completions (saw {})",
                counter.load(Ordering::Acquire)
            );
        }
        std::thread::yield_now();
    }
    start.elapsed()
}

/// Enqueues `count` immediate work completions from `n_threads` concurrent
/// producer threads (as evenly split as `count` allows), each just
/// incrementing `counter` when dispatched. Used by the throughput bench to
/// sweep producer-thread counts, and by the stress binary to hammer the
/// proactor from multiple threads simultaneously. Returns once every
/// producer thread has finished *submitting* -- not once every completion
/// has been *dispatched*; pair with `await_completions` for the latter.
pub fn flood_enqueue_work(
    handle: &ProactorHandle<PlatformPort>,
    counter: &Arc<AtomicU64>,
    count: u64,
    n_threads: u64,
) {
    let per_thread = count / n_threads;
    let remainder = count % n_threads;

    std::thread::scope(|scope| {
        for t in 0..n_threads {
            let handle = handle.clone();
            let counter = Arc::clone(counter);
            let this_thread_count = per_thread + if t < remainder { 1 } else { 0 };
            scope.spawn(move || {
                for _ in 0..this_thread_count {
                    let counter = Arc::clone(&counter);
                    handle
                        .enqueue_work(move |_completion| {
                            counter.fetch_add(1, Ordering::AcqRel);
                        })
                        .expect("enqueue_work failed");
                }
            });
        }
    });
}

/// One simulated 60Hz "frame tick": schedules `deferred_per_frame` timer
/// completions (`CompletionKind::Timer`, standing in for e.g. respawn
/// timers or animation callbacks) and `jobs_per_frame` immediate work
/// completions (`CompletionKind::Job`, standing in for e.g. off-thread
/// physics/pathfinding results arriving) against `handle`, then waits for
/// all of them to dispatch before returning. Used by `engine_workload`'s
/// bench to measure whether a realistic per-frame load fits comfortably
/// inside a 16.6ms budget, not just raw synthetic throughput.
pub fn simulate_frame(
    handle: &ProactorHandle<PlatformPort>,
    deferred_per_frame: u64,
    jobs_per_frame: u64,
) -> Duration {
    let counter = Arc::new(AtomicU64::new(0));
    let expected = deferred_per_frame + jobs_per_frame;

    for _ in 0..deferred_per_frame {
        let counter = Arc::clone(&counter);
        handle
            .defer_for(
                Duration::from_millis(0),
                CompletionKind::Timer,
                0,
                move |_| {
                    counter.fetch_add(1, Ordering::AcqRel);
                },
            )
            .expect("defer_for failed");
    }
    for _ in 0..jobs_per_frame {
        let counter = Arc::clone(&counter);
        handle
            .enqueue_work(move |_| {
                counter.fetch_add(1, Ordering::AcqRel);
            })
            .expect("enqueue_work failed");
    }

    await_completions(&counter, expected, Duration::from_secs(5))
}
