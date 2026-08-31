//! Simulates a realistic per-frame game-engine load rather than raw
//! synthetic throughput: N deferred timer callbacks (respawns, animation
//! ticks, cooldowns) plus M immediate job completions (off-thread physics
//! or pathfinding results arriving) every simulated frame, repeated for a
//! full simulated second at 60Hz. Run with:
//!   cargo bench -p proactor-harness --bench engine_workload
//!
//! Why this exists alongside `throughput.rs`: raw throughput tells you the
//! ceiling, but a game loop cares about whether *this specific, bounded*
//! per-frame load reliably finishes well inside a 16.6ms budget -- a
//! proactor that wins on peak throughput but has a bad p99 tail under a
//! realistic frame's load is the wrong tradeoff for
//! "loadngo-proactor as the shared engine core", which is the actual
//! question this harness exists to help answer.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use proactor_harness::{new_platform_proactor, simulate_frame, spawn_pump};

const FRAME_BUDGET_MS: u128 = 16;

/// (deferred_per_frame, jobs_per_frame) scenarios, roughly spanning "quiet
/// frame" to "busy frame with a lot of concurrent gameplay in flight".
const SCENARIOS: &[(u64, u64)] = &[(4, 4), (16, 16), (64, 64)];

fn per_frame_load(c: &mut Criterion) {
    let mut group = c.benchmark_group("simulated per-frame load");

    for &(deferred, jobs) in SCENARIOS {
        let proactor = new_platform_proactor().expect("failed to construct platform proactor");
        let (handle, join) = spawn_pump(proactor);

        group.bench_with_input(
            BenchmarkId::new("deferred+jobs", format!("{deferred}+{jobs}")),
            &(deferred, jobs),
            |b, &(deferred, jobs)| {
                b.iter(|| {
                    let elapsed = simulate_frame(&handle, deferred, jobs);
                    if elapsed.as_millis() > FRAME_BUDGET_MS {
                        eprintln!(
                            "warning: {deferred}+{jobs} took {elapsed:?}, over the {FRAME_BUDGET_MS}ms/frame budget"
                        );
                    }
                });
            },
        );

        handle.stop().expect("stop failed");
        join.join().expect("pump thread panicked");
    }

    group.finish();
}

criterion_group!(benches, per_frame_load);
criterion_main!(benches);
