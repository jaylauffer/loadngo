//! A stable, plainly-named binary for `perf record`/flamegraph sampling --
//! criterion's own bench binaries get hash-suffixed names under
//! target/release/deps/, awkward to reference from a shell script. Runs
//! one of two realistic, long-running workloads in a tight loop for a
//! fixed wall-clock duration, so a profiler has something to sample
//! against. See scripts/profile-uring.sh and this crate's README.md for
//! what backend this exercises -- io_uring only makes sense to profile on
//! `dolores`; this compiles and runs anywhere, but the numbers are only
//! meaningful for whichever backend is actually live on the machine you
//! run it on.
//!
//! Run with:
//!   cargo run -p proactor-harness --release --bin profile-target -- frame [seconds] [deferred_per_frame] [jobs_per_frame]
//!   cargo run -p proactor-harness --release --bin profile-target -- flood [seconds] [threads] [batch]
//!
//! `frame` (default) mirrors benches/engine_workload.rs's simulated
//! per-frame load. `flood` mirrors benches/throughput.rs's concurrent
//! producer-thread sweep -- use this one to profile the multi-thread
//! contention cliff throughput.rs's own numbers surfaced (see README.md).

use proactor_harness::{
    await_completions, flood_enqueue_work, new_platform_proactor, simulate_frame, spawn_pump,
};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn run_frame_mode(args: impl Iterator<Item = String>) {
    let mut args = args;
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);
    let deferred_per_frame: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(64);
    let jobs_per_frame: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(64);

    println!(
        "profile-target(frame): {seconds}s of {deferred_per_frame}+{jobs_per_frame} per simulated frame against {}",
        std::any::type_name::<proactor_harness::PlatformPort>()
    );

    let proactor = new_platform_proactor().expect("failed to construct platform proactor");
    let (handle, join) = spawn_pump(proactor);

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut frames = 0u64;
    while Instant::now() < deadline {
        simulate_frame(&handle, deferred_per_frame, jobs_per_frame);
        frames += 1;
    }

    handle.stop().expect("stop failed");
    join.join().expect("pump thread panicked");
    println!("profile-target(frame): {frames} simulated frames completed");
}

fn run_flood_mode(args: impl Iterator<Item = String>) {
    let mut args = args;
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);
    let n_threads: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(2);
    let batch: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(10_000);

    println!(
        "profile-target(flood): {seconds}s of {n_threads} producer threads x {batch}-op batches against {}",
        std::any::type_name::<proactor_harness::PlatformPort>()
    );

    let proactor = new_platform_proactor().expect("failed to construct platform proactor");
    let (handle, join) = spawn_pump(proactor);

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut batches = 0u64;
    while Instant::now() < deadline {
        let counter = Arc::new(AtomicU64::new(0));
        flood_enqueue_work(&handle, &counter, batch, n_threads);
        await_completions(&counter, batch, Duration::from_secs(30));
        batches += 1;
    }

    handle.stop().expect("stop failed");
    join.join().expect("pump thread panicked");
    println!("profile-target(flood): {batches} batches of {batch} completed");
}

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("flood") => run_flood_mode(args),
        Some("frame") | None => run_frame_mode(args),
        Some(other) => {
            eprintln!("profile-target: unknown mode {other:?}, expected \"frame\" or \"flood\"");
            std::process::exit(2);
        }
    }
}
