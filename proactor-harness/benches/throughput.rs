//! Throughput sweep: sustained completions/sec as the number of concurrent
//! producer threads grows, for a fixed batch size per run. Run with:
//!   cargo bench -p proactor-harness --bench throughput
//!
//! The interesting question isn't just "what's the peak number" -- it's
//! whether throughput scales sensibly with producer count or falls over
//! under contention (e.g. on the `pending_ops`/`ring` lock split in the
//! io_uring backend, or the single completion-queue mutex every backend
//! shares via `post()`/`drain_completion()`).

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use proactor_harness::{await_completions, flood_enqueue_work, new_platform_proactor, spawn_pump};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

const BATCH: u64 = 10_000;
const THREAD_COUNTS: &[u64] = &[1, 2, 4, 8, 16];

fn sweep_producer_threads(c: &mut Criterion) {
    let mut group = c.benchmark_group("enqueue_work throughput");
    group.throughput(Throughput::Elements(BATCH));

    for &n_threads in THREAD_COUNTS {
        let proactor = new_platform_proactor().expect("failed to construct platform proactor");
        let (handle, join) = spawn_pump(proactor);

        group.bench_with_input(
            BenchmarkId::from_parameter(n_threads),
            &n_threads,
            |b, &n_threads| {
                b.iter(|| {
                    let counter = Arc::new(AtomicU64::new(0));
                    flood_enqueue_work(&handle, &counter, BATCH, n_threads);
                    await_completions(&counter, BATCH, Duration::from_secs(30));
                });
            },
        );

        handle.stop().expect("stop failed");
        join.join().expect("pump thread panicked");
    }

    group.finish();
}

criterion_group!(benches, sweep_producer_threads);
criterion_main!(benches);
