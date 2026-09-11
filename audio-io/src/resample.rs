//! Clock-drift-compensating resampler between an input device and an output
//! device that run on independent crystals.
//!
//! The live monitor used to copy captured samples straight into the output
//! callback through a plain ring. Two nominally-48 kHz devices never agree
//! exactly: if the input's clock is even 100 ppm slower, the ring loses ~5
//! samples a second, drains to empty, and the monitor falls silent for good
//! (seen 2026-09-09 in `sng-bass-blaster`). And a plain copy can't bridge
//! devices at different rates at all (a 16 kHz headset mic into 48 kHz
//! speakers).
//!
//! This reads the ring from the output callback at a ratio of
//! `input_rate / output_rate`, nudged by a slow PI controller that holds the
//! ring at a target fill. The nudge is bounded to a few hundred ppm -- far
//! below anything audible as pitch -- and interpolation is 4-point cubic
//! Hermite, which is clean for the near-unity ratios drift needs and adequate
//! for rate conversion in a monitoring path.
//!
//! Everything here runs inside the output callback: no allocation, no locks,
//! bounded work per sample. Progress is published through atomics.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// When the input callback last delivered audio, and how much.
///
/// The ring's fill, read at an arbitrary moment, jumps by a whole input burst
/// at every input callback and drains through every output callback. Where
/// the output callback lands relative to the input one slides slowly under
/// drift (a 26-second cycle at 400 ppm with 512-frame buffers), so a raw fill
/// reading carries a slow sawtooth the controller would chase. Adding the
/// audio the input device has captured since its last callback but not yet
/// delivered makes the reading continuous -- the timestamp approach real
/// drift bridges (JACK's `alsa_out`, zita-ajbridge) use. The first cut read
/// raw fill and failed a simulated +400 ppm clock for exactly this reason.
#[derive(Debug)]
pub(crate) struct InputClock {
    epoch: Instant,
    last_push_nanos: AtomicU64,
    last_burst: AtomicU64,
}

impl InputClock {
    pub(crate) fn new() -> Self {
        Self {
            epoch: Instant::now(),
            last_push_nanos: AtomicU64::new(0),
            last_burst: AtomicU64::new(0),
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    pub(crate) fn now_nanos(&self) -> u64 {
        self.epoch.elapsed().as_nanos() as u64
    }

    /// Called by the input callback after pushing `frames` samples.
    pub(crate) fn record_push(&self, frames: usize) {
        self.record_push_at(self.now_nanos(), frames);
    }

    pub(crate) fn record_push_at(&self, nanos: u64, frames: usize) {
        self.last_push_nanos.store(nanos, Ordering::Relaxed);
        self.last_burst.store(frames as u64, Ordering::Relaxed);
    }

    /// Input samples captured but not yet delivered at `now_nanos`, capped at
    /// two bursts so a stalled input doesn't read as a full ring.
    #[allow(clippy::cast_precision_loss)]
    fn in_flight(&self, now_nanos: u64, input_rate_hz: f64) -> f64 {
        let last = self.last_push_nanos.load(Ordering::Relaxed);
        let burst = self.last_burst.load(Ordering::Relaxed) as f64;
        let elapsed = now_nanos.saturating_sub(last) as f64 * 1e-9;
        (elapsed * input_rate_hz).min(burst * 2.0)
    }
}

/// Largest ratio correction the controller will apply, as a fraction.
/// Crystal drift between consumer devices is typically well under 100 ppm.
const MAX_CORRECTION: f64 = 1e-3;
/// Integral gain, per unit of relative fill error, per second.
const KI_PER_SECOND: f64 = 2e-4;
/// Proportional gain, per unit of relative fill error. The loop is
/// `d(error)/dt = (rate / target) * (drift - correction)`; with a 48 kHz,
/// 1536-sample target and the integral gain above that is a second-order
/// system with a natural frequency near 0.08 rad/s, and this gain puts its
/// damping near 0.7. (The first cut used 2e-4, damping ~0.04: a simulated
/// fast clock rang for minutes instead of settling.) Its share of fill
/// jitter is ~100 ppm, about 0.2 cents -- inaudible.
const KP: f64 = 3.5e-3;
/// Time constant of the fill measurement's smoothing. Fill is sampled at
/// whatever phase the two callbacks happen to be in, so it jitters by a
/// whole input burst; the controller only needs its slow trend.
const FILL_SMOOTHING_SECONDS: f64 = 0.5;
/// Past this multiple of the target the output side has stalled (or the
/// input burst jumped); drop back to the target rather than play old audio.
const OVERFILL_LIMIT: f64 = 4.0;

/// What the resampler has been doing, for diagnostics.
#[derive(Debug, Default)]
pub(crate) struct ResamplerStats {
    buffered_samples: AtomicU64,
    target_samples: AtomicU64,
    correction_ppb: AtomicI64,
    underruns: AtomicU64,
    overflows: AtomicU64,
}

/// A snapshot of [`ResamplerStats`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DriftStats {
    /// Input samples waiting in the ring (smoothed).
    pub buffered_samples: u64,
    /// The fill the controller is steering toward.
    pub target_samples: u64,
    /// Current ratio correction, in parts per million. Positive means the
    /// input is running fast relative to the output.
    pub correction_ppm: f64,
    /// Times the ring ran dry and the output re-primed.
    pub underruns: u64,
    /// Times the ring overfilled and was trimmed back to target.
    pub overflows: u64,
}

impl ResamplerStats {
    pub(crate) fn snapshot(&self) -> DriftStats {
        #[allow(clippy::cast_precision_loss)]
        let correction_ppm = self.correction_ppb.load(Ordering::Relaxed) as f64 / 1_000.0;
        DriftStats {
            buffered_samples: self.buffered_samples.load(Ordering::Relaxed),
            target_samples: self.target_samples.load(Ordering::Relaxed),
            correction_ppm,
            underruns: self.underruns.load(Ordering::Relaxed),
            overflows: self.overflows.load(Ordering::Relaxed),
        }
    }
}

/// A source of mono samples: the monitor ring's consumer in production, a
/// plain queue in tests.
pub(crate) trait SampleSource {
    fn available(&self) -> usize;
    fn pop(&mut self) -> Option<f32>;
}

impl SampleSource for rtrb::Consumer<f32> {
    fn available(&self) -> usize {
        self.slots()
    }

    fn pop(&mut self) -> Option<f32> {
        rtrb::Consumer::pop(self).ok()
    }
}

pub(crate) struct DriftResampler {
    nominal_ratio: f64,
    input_rate_hz: f64,
    output_rate_hz: f64,
    target_fill: f64,
    /// `history[1]` is the sample at the current integer position; the
    /// output lies `phase` of the way from `history[1]` to `history[2]`.
    history: [f32; 4],
    phase: f64,
    primed: bool,
    fill_estimate: f64,
    integral: f64,
    correction: f64,
    stats: Arc<ResamplerStats>,
}

impl DriftResampler {
    /// `target_fill_samples` is in *input* samples: how much audio to keep
    /// queued. It is the monitoring latency this stage adds, and must exceed
    /// the larger of one input burst and one output block (in input samples)
    /// or the ring empties between callbacks.
    pub(crate) fn new(
        input_rate_hz: u32,
        output_rate_hz: u32,
        target_fill_samples: usize,
        stats: Arc<ResamplerStats>,
    ) -> Self {
        #[allow(clippy::cast_precision_loss)]
        let target_fill = target_fill_samples.max(1) as f64;
        stats
            .target_samples
            .store(target_fill_samples as u64, Ordering::Relaxed);
        Self {
            nominal_ratio: f64::from(input_rate_hz) / f64::from(output_rate_hz.max(1)),
            input_rate_hz: f64::from(input_rate_hz),
            output_rate_hz: f64::from(output_rate_hz.max(1)),
            target_fill,
            history: [0.0; 4],
            phase: 0.0,
            primed: false,
            fill_estimate: 0.0,
            integral: 0.0,
            correction: 0.0,
            stats,
        }
    }

    /// Fills `out` (interleaved, `channels` wide) from `source`, applying
    /// `gain`. The same mono value goes to every channel. `now_nanos` is
    /// `clock`'s time at the start of this output callback.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    pub(crate) fn render(
        &mut self,
        source: &mut impl SampleSource,
        out: &mut [f32],
        channels: usize,
        gain: f32,
        clock: &InputClock,
        now_nanos: u64,
    ) {
        let channels = channels.max(1);
        let queued = source.available() as f64;
        let available = queued + clock.in_flight(now_nanos, self.input_rate_hz);
        let block_seconds = (out.len() / channels) as f64 / self.output_rate_hz;

        if !self.primed {
            if queued < self.target_fill + 4.0 {
                out.fill(0.0);
                self.publish(available);
                return;
            }
            // Start exactly at the target: whatever piled up before the
            // output started (the input opens first) is stale, and starting
            // above target would read as an error the controller then spends
            // seconds working off.
            // Measured the same way the controller measures (queued plus
            // captured-but-undelivered), or every start begins a burst high.
            let stale = (available - self.target_fill - 4.0).max(0.0) as usize;
            for _ in 0..stale {
                let _ = source.pop();
            }
            for slot in &mut self.history {
                *slot = source.pop().unwrap_or(0.0);
            }
            self.phase = 0.0;
            self.fill_estimate = self.target_fill;
            // The integral is kept across a re-prime after an underrun: it
            // holds the drift the controller has already learned.
            self.primed = true;
        } else if queued > self.target_fill * OVERFILL_LIMIT {
            let excess = (queued - self.target_fill) as usize;
            for _ in 0..excess {
                let _ = source.pop();
            }
            self.fill_estimate = self.target_fill;
            self.integral = 0.0;
            self.stats.overflows.fetch_add(1, Ordering::Relaxed);
        } else {
            let smoothing = 1.0 - (-block_seconds / FILL_SMOOTHING_SECONDS).exp();
            self.fill_estimate += (available - self.fill_estimate) * smoothing;
        }

        let error = (self.fill_estimate - self.target_fill) / self.target_fill;
        self.integral = (self.integral + error * KI_PER_SECOND * block_seconds)
            .clamp(-MAX_CORRECTION, MAX_CORRECTION);
        self.correction = (error * KP + self.integral).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        let step = self.nominal_ratio * (1.0 + self.correction);

        let frames = out.len() / channels;
        out[frames * channels..].fill(0.0);
        for index in 0..frames {
            self.phase += step;
            while self.phase >= 1.0 {
                let Some(next) = source.pop() else {
                    // Dry: go quiet and wait for the ring to refill to the
                    // target before resuming, rather than stutter.
                    self.primed = false;
                    self.stats.underruns.fetch_add(1, Ordering::Relaxed);
                    out[index * channels..].fill(0.0);
                    self.publish(source.available() as f64);
                    return;
                };
                self.history = [self.history[1], self.history[2], self.history[3], next];
                self.phase -= 1.0;
            }
            let value = hermite(self.history, self.phase as f32) * gain;
            out[index * channels..(index + 1) * channels].fill(value);
        }
        self.publish(self.fill_estimate);
    }

    fn publish(&self, fill: f64) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        self.stats
            .buffered_samples
            .store(fill.max(0.0) as u64, Ordering::Relaxed);
        #[allow(clippy::cast_possible_truncation)]
        self.stats
            .correction_ppb
            .store((self.correction * 1e9) as i64, Ordering::Relaxed);
    }
}

/// 4-point cubic Hermite (Catmull-Rom) between `y[1]` and `y[2]`.
fn hermite(y: [f32; 4], t: f32) -> f32 {
    let c0 = y[1];
    let c1 = 0.5 * (y[2] - y[0]);
    let c2 = y[0] - 2.5 * y[1] + 2.0 * y[2] - 0.5 * y[3];
    let c3 = 0.5 * (y[3] - y[0]) + 1.5 * (y[1] - y[2]);
    ((c3 * t + c2) * t + c1) * t + c0
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    impl SampleSource for VecDeque<f32> {
        fn available(&self) -> usize {
            self.len()
        }

        fn pop(&mut self) -> Option<f32> {
            self.pop_front()
        }
    }

    struct Run {
        output: Vec<f32>,
        stats: DriftStats,
        fills: Vec<u64>,
    }

    /// Drives an input callback and an output callback on independent
    /// clocks for `seconds`, feeding a sine at `tone_hz`.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    #[allow(clippy::too_many_arguments)]
    fn simulate(
        input_rate: u32,
        input_clock_error: f64,
        input_block: usize,
        output_rate: u32,
        output_block: usize,
        target: usize,
        seconds: f64,
        tone_hz: f64,
    ) -> Run {
        let stats = Arc::new(ResamplerStats::default());
        let mut resampler = DriftResampler::new(input_rate, output_rate, target, stats.clone());
        let clock = InputClock::new();
        let mut queue: VecDeque<f32> = VecDeque::new();
        let actual_input_rate = f64::from(input_rate) * (1.0 + input_clock_error);
        let input_period = input_block as f64 / actual_input_rate;
        let output_period = output_block as f64 / f64::from(output_rate);
        let (mut next_input, mut next_output): (f64, f64) = (0.0, output_period * 0.37);
        let mut sample_index = 0u64;
        let mut output = Vec::new();
        let mut block = vec![0.0f32; output_block * 2];
        let mut fills = Vec::new();
        while next_input.min(next_output) < seconds {
            if next_input <= next_output {
                for _ in 0..input_block {
                    let t = sample_index as f64 / f64::from(input_rate);
                    queue.push_back((2.0 * std::f64::consts::PI * tone_hz * t).sin() as f32 * 0.5);
                    sample_index += 1;
                }
                clock.record_push_at((next_input * 1e9) as u64, input_block);
                next_input += input_period;
            } else {
                resampler.render(
                    &mut queue,
                    &mut block,
                    2,
                    1.0,
                    &clock,
                    (next_output * 1e9) as u64,
                );
                output.extend(block.as_chunks::<2>().0.iter().map(|frame| frame[0]));
                fills.push(stats.snapshot().buffered_samples);
                next_output += output_period;
            }
        }
        Run {
            output,
            stats: stats.snapshot(),
            fills,
        }
    }

    /// Frequency by counting rising zero crossings over `samples`.
    #[allow(clippy::cast_precision_loss)]
    fn measured_hz(samples: &[f32], rate: u32) -> f64 {
        let crossings = samples
            .windows(2)
            .filter(|pair| pair[0] < 0.0 && pair[1] >= 0.0)
            .count();
        crossings as f64 * f64::from(rate) / samples.len() as f64
    }

    #[test]
    fn a_slow_input_clock_no_longer_drains_the_monitor_to_silence() {
        // The 2026-09-09 failure: input 300 ppm slow. A plain ring loses
        // ~14 samples a second and empties within a minute.
        let run = simulate(48_000, -300e-6, 512, 48_000, 512, 1_536, 600.0, 440.0);
        assert_eq!(run.stats.underruns, 0, "{:?}", run.stats);
        let settled = &run.fills[run.fills.len() / 2..];
        let (low, high) = (settled.iter().min().unwrap(), settled.iter().max().unwrap());
        assert!(*low > 400 && *high < 2_700, "fill wandered: {low}..{high}");
        assert!(
            (run.stats.correction_ppm + 300.0).abs() < 60.0,
            "{:?}",
            run.stats
        );
    }

    #[test]
    fn a_fast_input_clock_holds_the_target_instead_of_piling_up() {
        for (input_block, output_block, target) in [(512, 512, 1_536), (128, 256, 768)] {
            let run = simulate(
                48_000,
                400e-6,
                input_block,
                48_000,
                output_block,
                target,
                600.0,
                440.0,
            );
            assert_eq!(
                run.stats.underruns, 0,
                "{input_block}/{output_block}: {:?}",
                run.stats
            );
            assert_eq!(run.stats.overflows, 0);
            let settled = &run.fills[run.fills.len() / 2..];
            let high = *settled.iter().max().unwrap();
            assert!(high < (target as u64) * 2, "fill piled up to {high}");
            assert!(
                (run.stats.correction_ppm - 400.0).abs() < 60.0,
                "{:?}",
                run.stats
            );
        }
    }

    #[test]
    fn different_device_rates_keep_pitch() {
        // A 16 kHz headset mic into 48 kHz speakers, and 44.1 into 48.
        for (input_rate, input_block, output_block) in [(16_000, 160, 480), (44_100, 441, 512)] {
            let run = simulate(
                input_rate,
                0.0,
                input_block,
                48_000,
                output_block,
                input_block * 4,
                30.0,
                440.0,
            );
            assert_eq!(run.stats.underruns, 0, "{input_rate}: {:?}", run.stats);
            let tail = &run.output[run.output.len() - 48_000 * 10..];
            let hz = measured_hz(tail, 48_000);
            assert!(
                (hz - 440.0).abs() < 0.5,
                "{input_rate} Hz input played at {hz} Hz"
            );
        }
    }

    #[test]
    fn a_stalled_output_is_trimmed_back_rather_than_played_late() {
        let stats = Arc::new(ResamplerStats::default());
        let mut resampler = DriftResampler::new(48_000, 48_000, 1_000, stats.clone());
        let clock = InputClock::new();
        let mut queue: VecDeque<f32> = (0..1_600).map(|_| 0.25).collect();
        let mut block = vec![0.0f32; 512];
        resampler.render(&mut queue, &mut block, 1, 1.0, &clock, 0);
        assert_eq!(stats.snapshot().overflows, 0, "priming is not an overflow");
        // The output stalls while the input keeps delivering.
        queue.extend((0..6_000).map(|_| 0.25));
        resampler.render(&mut queue, &mut block, 1, 1.0, &clock, 0);
        assert_eq!(stats.snapshot().overflows, 1);
        assert!(queue.len() < 1_100, "still {} queued", queue.len());
    }

    #[test]
    fn an_empty_ring_outputs_silence_until_primed_then_recovers() {
        let stats = Arc::new(ResamplerStats::default());
        let mut resampler = DriftResampler::new(48_000, 48_000, 100, stats.clone());
        let mut queue: VecDeque<f32> = (0..50).map(|_| 0.5).collect();
        let mut block = vec![1.0f32; 64];
        resampler.render(&mut queue, &mut block, 1, 1.0, &InputClock::new(), 0);
        assert!(block.iter().all(|sample| *sample == 0.0));
        queue.extend((0..200).map(|_| 0.5));
        resampler.render(&mut queue, &mut block, 1, 1.0, &InputClock::new(), 0);
        assert!((block[40] - 0.5).abs() < 1e-6);
        // Drain it dry mid-block: the remainder must be silence, not stale.
        let mut long = vec![1.0f32; 1_000];
        resampler.render(&mut queue, &mut long, 1, 1.0, &InputClock::new(), 0);
        assert_eq!(stats.snapshot().underruns, 1);
        assert_eq!(*long.last().unwrap(), 0.0);
    }
}
