//! Live full-duplex monitoring: capture a selected input device (e.g. a USB
//! instrument interface), play it on a selected output device (e.g. the Mac
//! mini's speakers) with gain and mute, and tap the same captured signal for
//! a tuner and a recorder -- one input stream, never two (see
//! `loadngo/docs/AUDIO_IO.md` for the device race that rule avoids).
//!
//! This file is backend-neutral: the device I/O comes from
//! `crate::backend::platform` (CoreAudio on macOS, `cpal` elsewhere for now).
//! Between the two devices sits a [`DriftResampler`], so their independent
//! clocks can't drain the monitor to silence or pile up latency, and devices
//! at different sample rates can be paired.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::backend::{platform, Direction};
use crate::capabilities::SampleResolution;
use crate::error::AudioIoError;
use crate::recording::{recording_ring, RecordingTap};
use crate::resample::{DriftResampler, DriftStats, InputClock, ResamplerStats};

/// Mono samples the monitor ring can hold. Generous on purpose: the
/// resampler keeps it near its small target fill, and the headroom only
/// matters while an output device stalls.
const MONITOR_RING_CAPACITY: usize = 1 << 16;
/// Assumed callback size when a backend can't say ahead of time.
const ASSUMED_BUFFER_FRAMES: u32 = 1_024;

#[derive(Debug, Clone)]
pub struct LiveMonitorConfig {
    /// `None` selects the host's current default input device.
    pub input_device_name: Option<String>,
    /// `None` selects the host's current default output device.
    pub output_device_name: Option<String>,
    pub initial_gain: f32,
    pub initial_muted: bool,
    /// How many mono samples of capture history `drain_tap` can hold before
    /// older samples are dropped. `16_384` is a handful of `PitchDetector`
    /// windows' worth at typical sample rates.
    pub tap_capacity: usize,
    /// How many seconds of full-width interleaved audio the recording ring
    /// (see [`LiveMonitor::take_recording_tap`]) holds before the input
    /// callback starts dropping frames. It only fills while a recorder is
    /// armed and falling behind, so this is the longest disk stall a
    /// recording survives without a gap.
    pub recording_buffer_seconds: f32,
    /// Frames per device callback to ask for. Smaller means lower monitoring
    /// latency and more CPU wakeups. `None` keeps each device's own size.
    /// CoreAudio scopes this to the calling process.
    pub preferred_buffer_frames: Option<u32>,
}

impl Default for LiveMonitorConfig {
    fn default() -> Self {
        Self {
            input_device_name: None,
            output_device_name: None,
            initial_gain: 1.0,
            initial_muted: false,
            tap_capacity: 16_384,
            recording_buffer_seconds: 4.0,
            preferred_buffer_frames: None,
        }
    }
}

/// Owns a live input-to-output monitoring session. Dropping it stops both
/// devices' I/O.
pub struct LiveMonitor {
    gain: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
    /// Consumer end of the analysis tap. Behind a `Mutex` purely for
    /// interior mutability behind `drain_tap`'s `&self`; the audio callback
    /// holds the producer end and never touches this lock.
    tap_consumer: Mutex<rtrb::Consumer<f32>>,
    /// How many of the most recent samples `drain_tap` keeps; older ones are
    /// discarded there rather than in the audio callback.
    tap_capacity: usize,
    /// `None` while a recorder holds it; see `take_recording_tap`.
    recording_tap: Option<RecordingTap>,
    input_device_name: String,
    output_device_name: String,
    input_sample_rate_hz: u32,
    output_sample_rate_hz: u32,
    input_channels: u16,
    input_resolution: Option<SampleResolution>,
    drift: Arc<ResamplerStats>,
    // Declared output first so it stops before the input it reads from.
    output_stream: platform::Stream,
    input_stream: platform::Stream,
}

impl LiveMonitor {
    /// Opens both devices and starts monitoring immediately.
    pub fn start(config: LiveMonitorConfig) -> Result<Self, AudioIoError> {
        let LiveMonitorConfig {
            input_device_name,
            output_device_name,
            initial_gain,
            initial_muted,
            tap_capacity,
            recording_buffer_seconds,
            preferred_buffer_frames,
        } = config;

        let gain = Arc::new(AtomicU32::new(initial_gain.clamp(0.0, 4.0).to_bits()));
        let muted = Arc::new(AtomicBool::new(initial_muted));

        // The rings are sized from the input's shape, so learn it first and
        // insist the opened stream matches.
        let expected_input = platform::describe(Direction::Input, input_device_name.as_deref())?;
        let (mut monitor_producer, mut monitor_consumer) =
            rtrb::RingBuffer::<f32>::new(MONITOR_RING_CAPACITY);
        // Twice `tap_capacity` so a caller that misses a frame or two still
        // has room for fresh audio behind the backlog `drain_tap` discards.
        let (mut tap_producer, tap_consumer) = rtrb::RingBuffer::<f32>::new(tap_capacity * 2);
        let (mut recording_producer, recording_tap) = recording_ring(
            expected_input.channels,
            expected_input.sample_rate_hz,
            recording_buffer_seconds,
        );
        let clock = Arc::new(InputClock::new());

        let input_clock = clock.clone();
        let mut mono = Vec::with_capacity(16_384);
        let (input_stream, input_format) = platform::open_input(
            input_device_name.as_deref(),
            preferred_buffer_frames,
            Box::new(move |interleaved: &[f32], channels: usize| {
                recording_producer.push_interleaved(interleaved);
                downmix_to_mono(interleaved, channels, &mut mono);
                for &sample in &mono {
                    // A full ring drops the newest sample; the resampler's
                    // overfill trim means that only happens when the output
                    // side has stopped consuming.
                    let _ = monitor_producer.push(sample);
                    // Dropping the newest tap sample when the consumer is a
                    // full ring behind self-corrects on the next drain.
                    let _ = tap_producer.push(sample);
                }
                input_clock.record_push(mono.len());
            }),
        )?;
        if input_format.channels != expected_input.channels
            || input_format.sample_rate_hz != expected_input.sample_rate_hz
        {
            return Err(AudioIoError::Stream(format!(
                "{} changed format while opening ({} Hz x{} became {} Hz x{})",
                input_format.device_name,
                expected_input.sample_rate_hz,
                expected_input.channels,
                input_format.sample_rate_hz,
                input_format.channels
            )));
        }

        let expected_output = platform::describe(Direction::Output, output_device_name.as_deref())?;
        let drift = Arc::new(ResamplerStats::default());
        let mut resampler = DriftResampler::new(
            input_format.sample_rate_hz,
            expected_output.sample_rate_hz,
            target_fill(&input_format, &expected_output, preferred_buffer_frames),
            drift.clone(),
        );
        let output_gain = gain.clone();
        let output_muted = muted.clone();
        let (output_stream, output_format) = platform::open_output(
            output_device_name.as_deref(),
            preferred_buffer_frames,
            Box::new(move |out: &mut [f32], channels: usize| {
                let gain = if output_muted.load(Ordering::Relaxed) {
                    0.0
                } else {
                    f32::from_bits(output_gain.load(Ordering::Relaxed))
                };
                let now = clock.now_nanos();
                resampler.render(&mut monitor_consumer, out, channels, gain, &clock, now);
            }),
        )?;
        if output_format.sample_rate_hz != expected_output.sample_rate_hz {
            return Err(AudioIoError::Stream(format!(
                "{} changed sample rate while opening",
                output_format.device_name
            )));
        }

        Ok(Self {
            gain,
            muted,
            tap_consumer: Mutex::new(tap_consumer),
            tap_capacity,
            recording_tap: Some(recording_tap),
            input_device_name: input_format.device_name,
            output_device_name: output_format.device_name,
            input_sample_rate_hz: input_format.sample_rate_hz,
            output_sample_rate_hz: output_format.sample_rate_hz,
            input_channels: input_format.channels,
            input_resolution: input_format.resolution,
            drift,
            output_stream,
            input_stream,
        })
    }

    #[must_use]
    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }

    pub fn set_gain(&self, gain: f32) {
        self.gain
            .store(gain.clamp(0.0, 4.0).to_bits(), Ordering::Relaxed);
    }

    #[must_use]
    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    #[must_use]
    pub fn input_sample_rate_hz(&self) -> u32 {
        self.input_sample_rate_hz
    }

    /// The output device's rate. It may differ from the input's; the drift
    /// resampler converts between them.
    #[must_use]
    pub fn output_sample_rate_hz(&self) -> u32 {
        self.output_sample_rate_hz
    }

    /// The input stream's full channel count -- what a recording captures,
    /// as opposed to the mono `drain_tap` analysis signal.
    #[must_use]
    pub fn input_channels(&self) -> u16 {
        self.input_channels
    }

    /// The sample encoding the OS delivers the input stream in. On macOS this
    /// is always 32-bit float regardless of the converter; see
    /// [`crate::probe_input_capabilities`] for the physical format.
    #[must_use]
    pub fn input_stream_resolution(&self) -> Option<SampleResolution> {
        self.input_resolution
    }

    #[must_use]
    pub fn input_device_name(&self) -> &str {
        &self.input_device_name
    }

    #[must_use]
    pub fn output_device_name(&self) -> &str {
        &self.output_device_name
    }

    /// How the clock-drift controller between the two devices is doing.
    #[must_use]
    pub fn drift_stats(&self) -> DriftStats {
        self.drift.snapshot()
    }

    /// The monitoring delay the drift stage currently holds, in milliseconds.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn buffered_ms(&self) -> f32 {
        let stats = self.drift.snapshot();
        stats.buffered_samples as f32 * 1_000.0 / self.input_sample_rate_hz.max(1) as f32
    }

    /// `Some(reason)` once either device has failed (e.g. was unplugged).
    #[must_use]
    pub fn failure(&self) -> Option<String> {
        self.input_stream
            .failure()
            .map(|reason| format!("input: {reason}"))
            .or_else(|| {
                self.output_stream
                    .failure()
                    .map(|reason| format!("output: {reason}"))
            })
    }

    /// Missed IO deadlines reported by the OS across both devices (CoreAudio
    /// only; always zero on backends that don't report them).
    #[must_use]
    pub fn overloads(&self) -> u64 {
        self.input_stream.overloads() + self.output_stream.overloads()
    }

    /// Takes the lossless recording ring out of the monitor so a recorder
    /// can drain it on its own thread. `None` if already taken. The ring is
    /// idle until the recorder calls [`RecordingTap::arm`].
    pub fn take_recording_tap(&mut self) -> Option<RecordingTap> {
        self.recording_tap.take()
    }

    /// Gives a recording ring back after a recording finishes, so the next
    /// one can take it. A tap from a *different* (since restarted) monitor
    /// is silently dropped instead: its producer is gone for good.
    pub fn return_recording_tap(&mut self, tap: RecordingTap) {
        if self.recording_tap.is_none() && !tap.is_abandoned() {
            self.recording_tap = Some(tap);
        }
    }

    /// Appends mono samples captured since the last call (at
    /// `input_sample_rate_hz`) to `out`. Intended to feed a
    /// [`crate::pitch::PitchDetector`] once per UI frame; safe to call at
    /// any rate, older samples are dropped once `tap_capacity` is exceeded.
    pub fn drain_tap(&self, out: &mut Vec<f32>) {
        let Ok(mut consumer) = self.tap_consumer.lock() else {
            return;
        };
        // Keep only the most recent `tap_capacity` samples. Discarding here
        // rather than in the input callback keeps the callback's work
        // proportional to the block it was handed.
        let skip = consumer.slots().saturating_sub(self.tap_capacity);
        for _ in 0..skip {
            let _ = consumer.pop();
        }
        while let Ok(sample) = consumer.pop() {
            out.push(sample);
        }
    }
}

/// The ring fill the drift resampler holds, in input samples: at least one
/// input burst plus one output block (converted to input samples), with a
/// quarter again as margin. It is monitoring latency, so no bigger than it
/// needs to be. The output device hasn't been opened yet when this runs, so
/// a requested buffer size stands in for the size it will switch to.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn target_fill(
    input: &crate::backend::StreamFormat,
    output: &crate::backend::StreamFormat,
    preferred_buffer_frames: Option<u32>,
) -> usize {
    let input_burst = f64::from(input.buffer_frames.unwrap_or(ASSUMED_BUFFER_FRAMES));
    let output_frames = preferred_buffer_frames
        .or(output.buffer_frames)
        .unwrap_or(ASSUMED_BUFFER_FRAMES);
    let output_block = f64::from(output_frames) * f64::from(input.sample_rate_hz)
        / f64::from(output.sample_rate_hz.max(1));
    ((input_burst + output_block) * 1.25).ceil() as usize
}

/// Averages interleaved multi-channel frames down to mono, replacing the
/// contents of `out`.
#[allow(clippy::cast_precision_loss)]
fn downmix_to_mono(data: &[f32], channels: usize, out: &mut Vec<f32>) {
    out.clear();
    if channels <= 1 {
        out.extend_from_slice(data);
        return;
    }
    out.reserve(data.len() / channels);
    for frame in data.chunks_exact(channels) {
        out.push(frame.iter().sum::<f32>() / channels as f32);
    }
}
