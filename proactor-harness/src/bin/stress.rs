//! Black-box concurrent stress tool: many threads hammering
//! enqueue/defer/register/deregister/wake simultaneously against a single
//! live proactor, for a fixed duration, with a watchdog that fails loudly
//! on a real hang instead of quietly wedging the terminal forever.
//!
//! This is a complementary, empirical check alongside
//! `loadngo-proactor/tests/loom_pending_ops_pattern.rs`'s exhaustive
//! model-checking -- loom proves the *modeled pattern* can't deadlock
//! across every interleaving it explores, but it models the pattern in
//! isolation (see that file's module doc for why it can't drive real
//! io_uring/kqueue syscalls). This binary instead drives the real,
//! complete `PlatformPort` -- real syscalls, real kernel scheduling, real
//! fds -- for a cheap, ongoing sanity check that the actual implementation
//! matches what the model says it should do. Neither replaces the other.
//!
//! Run with:
//!   cargo run -p proactor-harness --release --bin stress -- [seconds] [threads]
//! Defaults: 10 seconds, 8 threads. Exits non-zero (via the watchdog panic)
//! if the run doesn't finish within 2x the requested duration.

use loadngo_proactor::CompletionKind;
use proactor_harness::{new_platform_proactor, spawn_pump};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let seconds: u64 = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let n_threads: u64 = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);

    println!("stress: {n_threads} threads for {seconds}s against {}", std::any::type_name::<proactor_harness::PlatformPort>());

    let proactor = new_platform_proactor().expect("failed to construct platform proactor");
    let (handle, join) = spawn_pump(proactor);

    let dispatched = Arc::new(AtomicU64::new(0));
    let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(seconds);

    // Watchdog: if the whole run (including the final joins below) takes
    // more than 2x the requested duration, something is genuinely stuck --
    // abort loudly rather than hanging the terminal/CI job forever.
    let watchdog_deadline = Instant::now() + Duration::from_secs(seconds * 2 + 5);
    let watchdog = std::thread::spawn(move || {
        while Instant::now() < watchdog_deadline {
            std::thread::sleep(Duration::from_millis(200));
        }
        eprintln!("stress: WATCHDOG TRIPPED -- run did not finish in time, likely a hang");
        std::process::exit(1);
    });

    std::thread::scope(|scope| {
        for worker_id in 0..n_threads {
            let handle = handle.clone();
            let dispatched = Arc::clone(&dispatched);
            let stop_flag = Arc::clone(&stop_flag);
            scope.spawn(move || {
                let mut fds: Vec<[i32; 2]> = Vec::new();
                let mut rng_state: u64 = 0x9E3779B97F4A7C15u64.wrapping_add(worker_id);
                let mut next = || {
                    rng_state ^= rng_state << 13;
                    rng_state ^= rng_state >> 7;
                    rng_state ^= rng_state << 17;
                    rng_state
                };

                while !stop_flag.load(Ordering::Relaxed) {
                    match next() % 4 {
                        0 => {
                            let dispatched = Arc::clone(&dispatched);
                            handle
                                .enqueue_work(move |_| {
                                    dispatched.fetch_add(1, Ordering::Relaxed);
                                })
                                .expect("enqueue_work failed");
                        }
                        1 => {
                            let dispatched = Arc::clone(&dispatched);
                            handle
                                .defer_for(Duration::from_millis(0), CompletionKind::Timer, 0, move |_| {
                                    dispatched.fetch_add(1, Ordering::Relaxed);
                                })
                                .expect("defer_for failed");
                        }
                        #[cfg(unix)]
                        2 => {
                            let mut raw = [0i32; 2];
                            let rc = unsafe { libc::pipe(raw.as_mut_ptr()) };
                            if rc == 0 {
                                let [read_fd, _write_fd] = raw;
                                if handle
                                    .register_readable(read_fd, read_fd as u64, |_| {})
                                    .is_ok()
                                {
                                    fds.push(raw);
                                }
                            }
                        }
                        #[cfg(unix)]
                        3 => {
                            if let Some([read_fd, write_fd]) = fds.pop() {
                                let _ = handle.deregister_readable(read_fd, read_fd as u64);
                                unsafe {
                                    libc::close(read_fd);
                                    libc::close(write_fd);
                                }
                            } else {
                                let _ = handle.wake();
                            }
                        }
                        #[cfg(not(unix))]
                        2..=3 => {
                            let _ = handle.wake();
                        }
                        _ => unreachable!("next() % 4 is always in 0..=3"),
                    }
                }

                #[cfg(unix)]
                for [read_fd, write_fd] in fds {
                    let _ = handle.deregister_readable(read_fd, read_fd as u64);
                    unsafe {
                        libc::close(read_fd);
                        libc::close(write_fd);
                    }
                }
            });
        }

        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        stop_flag.store(true, Ordering::Relaxed);
    });

    handle.stop().expect("stop failed");
    join.join().expect("pump thread panicked");

    println!(
        "stress: completed cleanly, {} completions dispatched over {seconds}s across {n_threads} threads",
        dispatched.load(Ordering::Relaxed)
    );

    // Finished cleanly before the watchdog's deadline -- drop it without
    // joining (it's a detached sleep loop, nothing to clean up) rather
    // than blocking here for however long is left on its timer.
    drop(watchdog);
}
