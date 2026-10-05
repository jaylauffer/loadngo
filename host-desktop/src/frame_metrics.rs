//! Frame pacing metrics, opt-in with `LOADNGO_FRAME_METRICS=1` (on Android,
//! `adb shell setprop debug.loadngo_frame_metrics 1`). One instrument for
//! every host that records frames, so a session on any of them prints the
//! same `[loadngo-frame]` report and the numbers compare directly.
//!
//! This measures *scheduling* behaviour -- real frame intervals, host thread
//! spawns, wake deliveries -- and deliberately not CPU or context-switch
//! counts. A clean load profile does not prove a scheduling change is correct:
//! the Android proactor migration (2026-09-03) shipped a severe frame-pacing
//! regression with healthy-looking CPU and context-switch numbers throughout,
//! and only play-testing caught it. Intervals have to be measured directly.
//!
//! Everything on the recording path is a relaxed atomic, so a host can call it
//! while holding its own state lock. `maybe_report` does real I/O and must be
//! called with no lock held, so the instrument does not perturb what it
//! measures.
//!
//! `host_thread_spawns` should read zero on every host since the proactor
//! migrations: no host spawns a thread to schedule a frame any more, and that
//! zero *is* the evidence. Anything that reintroduces one should call
//! `record_thread_spawn`, and the report will say so.

// A host that records frames uses all of this; the others compile it unused.
#![cfg_attr(
    not(any(target_os = "android", target_os = "ios", target_os = "linux")),
    allow(dead_code)
)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Upper bounds, in microseconds, for the interval histogram. Chosen around
/// the 60Hz (16_667us) and 120Hz (8_333us) frame budgets so the buckets
/// either side of a target actually distinguish "on time" from "one frame
/// late", rather than smearing both into one wide bucket.
const BUCKET_BOUNDS_US: [u64; 13] = [
    4_000, 8_000, 12_000, 15_000, 16_000, 17_000, 18_000, 20_000, 25_000, 33_000, 50_000, 100_000,
    200_000,
];

struct FrameMetrics {
    frames: AtomicU64,
    interval_sum_us: AtomicU64,
    interval_sq_sum: AtomicU64,
    interval_min_us: AtomicU64,
    interval_max_us: AtomicU64,
    buckets: [AtomicU64; BUCKET_BOUNDS_US.len() + 1],
    thread_spawns: AtomicU64,
    wakes: AtomicU64,
}

impl FrameMetrics {
    const fn new() -> Self {
        #[allow(clippy::declare_interior_mutable_const)]
        const ZERO: AtomicU64 = AtomicU64::new(0);
        Self {
            frames: AtomicU64::new(0),
            interval_sum_us: AtomicU64::new(0),
            interval_sq_sum: AtomicU64::new(0),
            interval_min_us: AtomicU64::new(u64::MAX),
            interval_max_us: AtomicU64::new(0),
            buckets: [ZERO; BUCKET_BOUNDS_US.len() + 1],
            thread_spawns: AtomicU64::new(0),
            wakes: AtomicU64::new(0),
        }
    }
}

static METRICS: FrameMetrics = FrameMetrics::new();

pub(crate) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        crate::debug_config_value("LOADNGO_FRAME_METRICS")
            .map(|value| {
                let value = value.trim().to_ascii_lowercase();
                matches!(value.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(false)
    })
}

/// How many frames between reports. 300 frames is ~5s at 60Hz.
fn report_every() -> u64 {
    static EVERY: OnceLock<u64> = OnceLock::new();
    *EVERY.get_or_init(|| {
        crate::debug_config_value("LOADNGO_FRAME_METRICS_EVERY")
            .and_then(|value| value.trim().parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(300)
    })
}

fn start() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// Records one frame interval. Atomics only.
pub(crate) fn record_interval(dt: Duration) {
    if !enabled() {
        return;
    }
    let _ = start();
    let us = dt.as_micros().min(u128::from(u64::MAX)) as u64;

    METRICS.frames.fetch_add(1, Ordering::Relaxed);
    METRICS.interval_sum_us.fetch_add(us, Ordering::Relaxed);
    // Clamped before squaring, so this cannot overflow across any realistic
    // session length.
    let clamped = us.min(1_000_000);
    METRICS
        .interval_sq_sum
        .fetch_add(clamped.saturating_mul(clamped), Ordering::Relaxed);
    METRICS.interval_min_us.fetch_min(us, Ordering::Relaxed);
    METRICS.interval_max_us.fetch_max(us, Ordering::Relaxed);

    let index = BUCKET_BOUNDS_US
        .iter()
        .position(|bound| us < *bound)
        .unwrap_or(BUCKET_BOUNDS_US.len());
    METRICS.buckets[index].fetch_add(1, Ordering::Relaxed);
}

/// Deliberately retained with no caller; see the module comment.
#[allow(dead_code)]
pub(crate) fn record_thread_spawn() {
    if enabled() {
        METRICS.thread_spawns.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn record_wake() {
    if enabled() {
        METRICS.wakes.fetch_add(1, Ordering::Relaxed);
    }
}

/// Hands a report line to `sink` if one is due. Call with no host lock held.
pub(crate) fn maybe_report(sink: impl FnOnce(&str)) {
    if !enabled() {
        return;
    }
    let frames = METRICS.frames.load(Ordering::Relaxed);
    if frames == 0 || !frames.is_multiple_of(report_every()) {
        return;
    }
    let counts: [u64; BUCKET_BOUNDS_US.len() + 1] =
        std::array::from_fn(|index| METRICS.buckets[index].load(Ordering::Relaxed));
    let line = format_report(
        frames,
        start().elapsed().as_secs_f64(),
        [
            METRICS.interval_sum_us.load(Ordering::Relaxed),
            METRICS.interval_sq_sum.load(Ordering::Relaxed),
            METRICS.interval_min_us.load(Ordering::Relaxed),
            METRICS.interval_max_us.load(Ordering::Relaxed),
        ],
        &counts,
        METRICS.thread_spawns.load(Ordering::Relaxed),
        METRICS.wakes.load(Ordering::Relaxed),
    );
    sink(&line);
}

fn format_report(
    frames: u64,
    elapsed: f64,
    [sum_us, sq_sum, min_us, max_us]: [u64; 4],
    counts: &[u64],
    spawns: u64,
    wakes: u64,
) -> String {
    let mean_us = sum_us as f64 / frames as f64;
    let variance = (sq_sum as f64 / frames as f64) - (mean_us * mean_us);
    let stddev_us = if variance > 0.0 { variance.sqrt() } else { 0.0 };
    let pct = |target: f64| -> String {
        let want = (frames as f64 * target).ceil() as u64;
        let mut running = 0u64;
        for (index, count) in counts.iter().enumerate() {
            running += count;
            if running >= want {
                return match BUCKET_BOUNDS_US.get(index) {
                    Some(bound) => format!("<{:.1}ms", *bound as f64 / 1000.0),
                    None => format!(
                        ">={:.1}ms",
                        BUCKET_BOUNDS_US[BUCKET_BOUNDS_US.len() - 1] as f64 / 1000.0
                    ),
                };
            }
        }
        "n/a".to_string()
    };
    format!(
        "[loadngo-frame] frames={frames} elapsed={elapsed:.1}s fps={:.1} \
         interval mean={:.2}ms stddev={:.2}ms min={:.2}ms max={:.2}ms \
         p50={} p95={} p99={} host_thread_spawns={spawns} ({:.1}/frame) wakes={wakes}",
        frames as f64 / elapsed.max(f64::EPSILON),
        mean_us / 1000.0,
        stddev_us / 1000.0,
        min_us as f64 / 1000.0,
        max_us as f64 / 1000.0,
        pct(0.50),
        pct(0.95),
        pct(0.99),
        spawns as f64 / frames as f64,
    )
}

#[cfg(test)]
mod tests {
    use super::{format_report, BUCKET_BOUNDS_US};

    #[test]
    fn the_report_keeps_the_shared_format_and_bucketed_percentiles() {
        // 100 frames: 98 at ~16.7 ms (the <17 ms bucket), 2 at 40 ms (<50 ms).
        let mut counts = [0u64; BUCKET_BOUNDS_US.len() + 1];
        counts[5] = 98;
        counts[10] = 2;
        let sum = 98 * 16_700 + 2 * 40_000;
        let sq = 98 * 16_700u64 * 16_700 + 2 * 40_000u64 * 40_000;
        let line = format_report(100, 1.75, [sum, sq, 16_600, 40_000], &counts, 0, 99);
        assert!(line.starts_with("[loadngo-frame] frames=100 elapsed=1.8s fps=57.1 "));
        assert!(line.contains("min=16.60ms max=40.00ms"));
        assert!(line.contains("p50=<17.0ms p95=<17.0ms p99=<50.0ms"));
        assert!(line.ends_with("host_thread_spawns=0 (0.0/frame) wakes=99"));
    }
}
