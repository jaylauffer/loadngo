//! Latency microbenchmarks: the per-operation overhead of the proactor's
//! own machinery, in isolation from any real I/O. Run with:
//!   cargo bench -p proactor-harness --bench latency
//!
//! Each backend only benchmarks itself (there's exactly one `PlatformPort`
//! per platform) -- run this once on `dolores` for io_uring numbers and
//! once on macOS for kqueue numbers, then compare the two reports by hand;
//! criterion doesn't span machines for you.

use criterion::{criterion_group, criterion_main, Criterion};
use loadngo_proactor::CompletionKind;
use proactor_harness::{new_platform_proactor, spawn_pump};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// enqueue_work -> dispatch round trip, single producer, no contention.
/// This is the floor: the cheapest possible unit of work the proactor can
/// carry, useful as a baseline every other number here should be compared
/// against.
fn enqueue_dispatch_round_trip(c: &mut Criterion) {
    let proactor = new_platform_proactor().expect("failed to construct platform proactor");
    let (handle, join) = spawn_pump(proactor);
    let counter = Arc::new(AtomicU64::new(0));

    c.bench_function("enqueue_work round trip (1 producer)", |b| {
        b.iter(|| {
            let target = counter.load(Ordering::Acquire) + 1;
            let for_closure = Arc::clone(&counter);
            handle
                .enqueue_work(move |_| {
                    for_closure.fetch_add(1, Ordering::AcqRel);
                })
                .expect("enqueue_work failed");
            while counter.load(Ordering::Acquire) < target {
                std::hint::spin_loop();
            }
        });
    });

    handle.stop().expect("stop failed");
    join.join().expect("pump thread panicked");
}

/// `defer_for`'s scheduling overhead with a near-zero delay -- how much
/// latency the deferred-queue machinery itself adds on top of the
/// underlying timeout mechanism, separate from however long the requested
/// delay actually is.
fn defer_near_zero_delay(c: &mut Criterion) {
    let proactor = new_platform_proactor().expect("failed to construct platform proactor");
    let (handle, join) = spawn_pump(proactor);
    let counter = Arc::new(AtomicU64::new(0));

    c.bench_function("defer_for(0ms) round trip", |b| {
        b.iter(|| {
            let target = counter.load(Ordering::Acquire) + 1;
            let for_closure = Arc::clone(&counter);
            handle
                .defer_for(Duration::from_millis(0), CompletionKind::Timer, 0, move |_| {
                    for_closure.fetch_add(1, Ordering::AcqRel);
                })
                .expect("defer_for failed");
            while counter.load(Ordering::Acquire) < target {
                std::hint::spin_loop();
            }
        });
    });

    handle.stop().expect("stop failed");
    join.join().expect("pump thread panicked");
}

/// `register_readable`/`deregister` overhead on a real fd (a pipe, cheap
/// and portable to create). This is exactly the code path the real
/// deadlock this workspace already fixed once lived in (see
/// `loadngo-proactor/src/uring.rs`'s `pending_ops` doc comment) -- worth
/// watching for latency regressions specifically here, not just overall
/// throughput, since a slow fast-path could indicate the try_lock/queue
/// split isn't behaving as designed.
#[cfg(unix)]
fn register_deregister_round_trip(c: &mut Criterion) {
    let proactor = new_platform_proactor().expect("failed to construct platform proactor");
    let (handle, join) = spawn_pump(proactor);

    c.bench_function("register_readable + deregister (uncontended)", |b| {
        b.iter(|| {
            let mut fds = [0i32; 2];
            let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
            assert_eq!(rc, 0, "pipe() failed");
            let [read_fd, write_fd] = fds;

            handle
                .register_readable(read_fd, read_fd as u64, |_| {})
                .expect("register_readable failed");
            handle
                .deregister_readable(read_fd, read_fd as u64)
                .expect("deregister_readable failed");

            unsafe {
                libc::close(read_fd);
                libc::close(write_fd);
            }
        });
    });

    handle.stop().expect("stop failed");
    join.join().expect("pump thread panicked");
}

#[cfg(unix)]
criterion_group!(
    benches,
    enqueue_dispatch_round_trip,
    defer_near_zero_delay,
    register_deregister_round_trip
);
#[cfg(not(unix))]
criterion_group!(benches, enqueue_dispatch_round_trip, defer_near_zero_delay);

criterion_main!(benches);
