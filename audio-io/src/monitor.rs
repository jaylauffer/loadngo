//! Live full-duplex monitoring: read frames from a selected input device
//! (e.g. a USB instrument interface), apply gain/mute, and write them to a
//! selected output device (e.g. the Mac mini's speakers) with minimal
//! latency. Also taps the same captured signal for non-real-time consumers
//! like a tuner's pitch detector, so only one input stream is ever open --
//! see this crate's `docs` reference in `loadngo/docs/AUDIO_IO.md` for why
//! that matters (the exact multi-`OutputStream` device race
//! `loadngo-host-desktop`'s `AudioMixer` was built to close on the
//! playback side).
//!
//! The worker thread's lifecycle (block until told to stop) is a
//! `loadngo_proactor::Proactor<ChannelPort>`, the same ownership pattern
//! `network/src/bin/task-node.rs` uses: the thread that owns the resource
//! (here, the `cpal` streams, kept alive on this thread's stack since
//! `cpal::Stream` isn't guaranteed `Send`) calls `run_until_stopped()`,
//! and `LiveMonitor::drop` calls `stop()` on a cloned `ProactorHandle` to
//! end it -- reusing loadngo's own event-processing primitive instead of a
//! bespoke stop channel. The one-time "did device setup succeed"
//! handback is still a plain `mpsc` channel: it has to resolve *before*
//! the worker's `Proactor` exists to be waited on, which is exactly the
//! case a reactor isn't the right tool for.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use loadngo_proactor::{ChannelPort, Proactor, ProactorHandle};

use crate::capabilities::SampleResolution;
use crate::error::AudioIoError;
use crate::recording::{recording_ring, RecordingProducer, RecordingTap};

/// Ring buffer capacity between the input and output callbacks, in mono
/// samples. At a typical `44_100`/`48_000` Hz device this is roughly
/// 90-185 ms of headroom against the two devices' independent hardware
/// clocks drifting apart between callbacks -- enough to avoid audible
/// underrun/overrun for a monitoring session of ordinary length, at the
/// cost of that much added latency. Not adaptive; a future revision could
/// resample to fully lock the two clocks together if the fixed buffer
/// proves audible in practice.
const MONITOR_RING_CAPACITY: usize = 8192;

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
        }
    }
}

struct MonitorReady {
    sample_rate_hz: u32,
    channels: u16,
    resolution: Option<SampleResolution>,
    recording_tap: RecordingTap,
    input_device_name: String,
    output_device_name: String,
}

/// Owns a live input-to-output monitoring session on a dedicated worker
/// thread. Dropping it stops both streams and joins the thread.
pub struct LiveMonitor {
    gain: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
    /// Consumer end of the analysis tap. Wrapped in a `Mutex` purely for
    /// interior mutability behind `drain_tap`'s `&self`: this is an SPSC
    /// ring, and the only thread that ever locks it is the one calling
    /// `drain_tap`. The audio callback holds the producer end and never
    /// touches this lock -- which is the whole point, see `tap_capacity`.
    tap_consumer: Mutex<rtrb::Consumer<f32>>,
    /// How many of the most recent samples `drain_tap` keeps; older ones
    /// are discarded there rather than in the audio callback.
    tap_capacity: usize,
    input_sample_rate_hz: u32,
    input_channels: u16,
    input_resolution: Option<SampleResolution>,
    /// `None` while a recorder holds it; see `take_recording_tap`.
    recording_tap: Option<RecordingTap>,
    input_device_name: String,
    output_device_name: String,
    proactor_handle: ProactorHandle<ChannelPort>,
    worker: Option<JoinHandle<()>>,
}

impl LiveMonitor {
    /// Starts capture and playback immediately. Blocks briefly for the
    /// worker thread to open both devices and report success or failure --
    /// device/stream setup happens entirely on that thread (not here)
    /// since `cpal`'s device and stream types are not guaranteed `Send`
    /// across every backend.
    pub fn start(config: LiveMonitorConfig) -> Result<Self, AudioIoError> {
        let LiveMonitorConfig {
            input_device_name,
            output_device_name,
            initial_gain,
            initial_muted,
            tap_capacity,
            recording_buffer_seconds,
        } = config;

        let gain = Arc::new(AtomicU32::new(initial_gain.clamp(0.0, 4.0).to_bits()));
        let muted = Arc::new(AtomicBool::new(initial_muted));
        // Twice `tap_capacity` so a caller that misses a frame or two
        // still has room for fresh audio behind the backlog `drain_tap`
        // will discard.
        let (tap_producer, tap_consumer) = rtrb::RingBuffer::<f32>::new(tap_capacity * 2);

        let (ready_tx, ready_rx) = mpsc::channel::<Result<MonitorReady, AudioIoError>>();

        // Created here (not on the worker thread) since a `Proactor` and
        // its `ChannelPort` have no thread affinity -- only the `cpal`
        // device/stream objects built inside `open_streams` do. The worker
        // takes ownership of `proactor` itself (to run it); this thread
        // keeps `proactor_handle` to call `stop()` from `Drop`.
        let proactor = Proactor::new(ChannelPort::new());
        let proactor_handle = proactor.handle();

        let thread_gain = gain.clone();
        let thread_muted = muted.clone();
        let thread_tap_producer = tap_producer;

        let worker = thread::Builder::new()
            .name("loadngo-audio-io-monitor".to_string())
            .spawn(move || {
                run_monitor_thread(
                    input_device_name,
                    output_device_name,
                    thread_gain,
                    thread_muted,
                    thread_tap_producer,
                    recording_buffer_seconds,
                    ready_tx,
                    proactor,
                );
            })
            .map_err(|error| AudioIoError::Thread(error.to_string()))?;

        match ready_rx.recv() {
            Ok(Ok(ready)) => Ok(Self {
                gain,
                muted,
                tap_consumer: Mutex::new(tap_consumer),
                tap_capacity,
                input_sample_rate_hz: ready.sample_rate_hz,
                input_channels: ready.channels,
                input_resolution: ready.resolution,
                recording_tap: Some(ready.recording_tap),
                input_device_name: ready.input_device_name,
                output_device_name: ready.output_device_name,
                proactor_handle,
                worker: Some(worker),
            }),
            Ok(Err(error)) => {
                // Setup failed before `run_until_stopped` ever started;
                // `stop()` is a harmless no-op with nothing polling yet,
                // kept for symmetry with the `Drop` path.
                let _ = proactor_handle.stop();
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                let _ = proactor_handle.stop();
                let _ = worker.join();
                Err(AudioIoError::WorkerExited)
            }
        }
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

    #[must_use]
    pub fn input_device_name(&self) -> &str {
        &self.input_device_name
    }

    #[must_use]
    pub fn output_device_name(&self) -> &str {
        &self.output_device_name
    }

    /// Appends mono samples captured since the last call (at
    /// `input_sample_rate_hz`) to `out`. Intended to feed a
    /// [`crate::pitch::PitchDetector`] once per UI frame; safe to call at
    /// any rate, older samples are dropped once `tap_capacity` is exceeded.
    pub fn drain_tap(&self, out: &mut Vec<f32>) {
        let Ok(mut consumer) = self.tap_consumer.lock() else {
            return;
        };
        // Keep only the most recent `tap_capacity` samples. This discard
        // used to happen in the input callback (which also had to take a
        // lock to do it); doing it here keeps the callback lock-free and
        // its work proportional to the block it was handed rather than to
        // however far this consumer has fallen behind.
        let skip = consumer.slots().saturating_sub(self.tap_capacity);
        for _ in 0..skip {
            let _ = consumer.pop();
        }
        while let Ok(sample) = consumer.pop() {
            out.push(sample);
        }
    }
}

impl Drop for LiveMonitor {
    fn drop(&mut self) {
        let _ = self.proactor_handle.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_monitor_thread(
    input_device_name: Option<String>,
    output_device_name: Option<String>,
    gain: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
    tap_producer: rtrb::Producer<f32>,
    recording_buffer_seconds: f32,
    ready_tx: mpsc::Sender<Result<MonitorReady, AudioIoError>>,
    proactor: Proactor<ChannelPort>,
) {
    let outcome = open_streams(
        input_device_name.as_deref(),
        output_device_name.as_deref(),
        gain,
        muted,
        tap_producer,
        recording_buffer_seconds,
    );

    match outcome {
        Ok((input_stream, output_stream, ready)) => {
            if ready_tx.send(Ok(ready)).is_err() {
                return;
            }
            // Blocks until `LiveMonitor::drop`'s `proactor_handle.stop()`
            // wakes this. Both streams stay alive (and thus playing) for
            // exactly as long as this call is running.
            let _ = proactor.run_until_stopped();
            drop(input_stream);
            drop(output_stream);
        }
        Err(error) => {
            let _ = ready_tx.send(Err(error));
        }
    }
}

type OpenStreamsResult = Result<(cpal::Stream, cpal::Stream, MonitorReady), AudioIoError>;

fn open_streams(
    input_device_name: Option<&str>,
    output_device_name: Option<&str>,
    gain: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
    tap_producer: rtrb::Producer<f32>,
    recording_buffer_seconds: f32,
) -> OpenStreamsResult {
    let host = cpal::default_host();
    let input_device = resolve_device(true, &host, input_device_name)?;
    let output_device = resolve_device(false, &host, output_device_name)?;
    let input_device_name = input_device
        .name()
        .unwrap_or_else(|_| "unknown input".to_string());
    let output_device_name = output_device
        .name()
        .unwrap_or_else(|_| "unknown output".to_string());

    let input_supported = input_device
        .default_input_config()
        .map_err(|error| AudioIoError::Cpal(error.to_string()))?;
    let sample_rate = input_supported.sample_rate();
    let input_channels = input_supported.channels() as usize;
    let input_sample_format = input_supported.sample_format();
    let input_config: cpal::StreamConfig = input_supported.into();
    let (recording_producer, recording_tap) = recording_ring(
        input_config.channels,
        sample_rate.0,
        recording_buffer_seconds,
    );

    let output_supported = output_device
        .default_output_config()
        .map_err(|error| AudioIoError::Cpal(error.to_string()))?;
    let output_channels = output_supported.channels() as usize;
    let output_sample_format = output_supported.sample_format();
    // Deliberately built at the *input's* sample rate rather than the
    // output device's own default -- see `MONITOR_RING_CAPACITY`'s note on
    // clock drift. If the output device can't run at this rate, `cpal`
    // surfaces that as a `Stream` build error below rather than silently
    // resampling; picking a matching pair of devices is on the caller.
    let output_config = cpal::StreamConfig {
        channels: output_supported.channels(),
        sample_rate,
        buffer_size: cpal::BufferSize::Default,
    };

    let (producer, consumer) = rtrb::RingBuffer::<f32>::new(MONITOR_RING_CAPACITY);

    let input_stream = build_input_stream(
        &input_device,
        &input_config,
        input_sample_format,
        input_channels,
        producer,
        tap_producer,
        recording_producer,
    )?;
    let output_stream = build_output_stream(
        &output_device,
        &output_config,
        output_sample_format,
        output_channels,
        consumer,
        gain,
        muted,
    )?;

    input_stream
        .play()
        .map_err(|error| AudioIoError::Stream(error.to_string()))?;
    output_stream
        .play()
        .map_err(|error| AudioIoError::Stream(error.to_string()))?;

    Ok((
        input_stream,
        output_stream,
        MonitorReady {
            sample_rate_hz: sample_rate.0,
            channels: input_config.channels,
            resolution: SampleResolution::from_cpal(input_sample_format),
            recording_tap,
            input_device_name,
            output_device_name,
        },
    ))
}

pub(crate) fn resolve_device(
    is_input: bool,
    host: &cpal::Host,
    requested_name: Option<&str>,
) -> Result<cpal::Device, AudioIoError> {
    match requested_name {
        Some(name) => {
            let mut devices = if is_input {
                host.input_devices()
            } else {
                host.output_devices()
            }
            .map_err(|error| AudioIoError::Cpal(error.to_string()))?;
            devices
                .find(|device| device.name().map(|n| n == name).unwrap_or(false))
                .ok_or_else(|| {
                    if is_input {
                        AudioIoError::InputDeviceNotFound(name.to_string())
                    } else {
                        AudioIoError::OutputDeviceNotFound(name.to_string())
                    }
                })
        }
        None => {
            let device = if is_input {
                host.default_input_device()
            } else {
                host.default_output_device()
            };
            device.ok_or(if is_input {
                AudioIoError::NoDefaultInputDevice
            } else {
                AudioIoError::NoDefaultOutputDevice
            })
        }
    }
}

/// Averages interleaved multi-channel frames down to mono, appending the
/// result to `out` (which is cleared first).
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

fn build_input_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    channels: usize,
    mut producer: rtrb::Producer<f32>,
    mut tap_producer: rtrb::Producer<f32>,
    mut recording: RecordingProducer,
) -> Result<cpal::Stream, AudioIoError> {
    let publish = move |mono: &[f32]| {
        for &sample in mono {
            // Overwrite-oldest-on-full would need a different ring buffer
            // shape; dropping the newest sample under sustained overrun
            // (output side unable to keep up) is the simpler, still
            // acceptable failure mode for a monitoring tool.
            let _ = producer.push(sample);
            // Second lock-free ring rather than a shared `Mutex<VecDeque>`:
            // locking (and allocating) inside an audio callback is a
            // real-time violation, and under contention it stalls capture
            // long enough to drain the monitoring ring. Dropping the newest
            // tap sample when the consumer has fallen a full ring behind is
            // the acceptable failure here -- it self-corrects on the next
            // `drain_tap`.
            let _ = tap_producer.push(sample);
        }
    };

    let error_callback = |error: cpal::StreamError| {
        eprintln!("loadngo-audio-io: input stream error: {error}");
    };

    let stream = match sample_format {
        cpal::SampleFormat::F32 => {
            let mut publish = publish;
            let mut mono = Vec::new();
            device.build_input_stream(
                config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    recording.push_interleaved(data);
                    downmix_to_mono(data, channels, &mut mono);
                    publish(&mono);
                },
                error_callback,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let mut publish = publish;
            let mut float = Vec::new();
            let mut mono = Vec::new();
            device.build_input_stream(
                config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    float.clear();
                    // 2^15, not i16::MAX: the same power-of-two scale
                    // CoreAudio uses, so a recorder can map samples back to
                    // integers exactly with one rule on every platform.
                    float.extend(data.iter().map(|&s| f32::from(s) / 32_768.0));
                    recording.push_interleaved(&float);
                    downmix_to_mono(&float, channels, &mut mono);
                    publish(&mono);
                },
                error_callback,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let mut publish = publish;
            let mut float = Vec::new();
            let mut mono = Vec::new();
            device.build_input_stream(
                config,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    float.clear();
                    float.extend(data.iter().map(|&s| (f32::from(s) - 32_768.0) / 32_768.0));
                    recording.push_interleaved(&float);
                    downmix_to_mono(&float, channels, &mut mono);
                    publish(&mono);
                },
                error_callback,
                None,
            )
        }
        other => return Err(AudioIoError::UnsupportedSampleFormat(other)),
    };

    stream.map_err(|error| AudioIoError::Stream(error.to_string()))
}

fn build_output_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    channels: usize,
    mut consumer: rtrb::Consumer<f32>,
    gain: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
) -> Result<cpal::Stream, AudioIoError> {
    let error_callback = |error: cpal::StreamError| {
        eprintln!("loadngo-audio-io: output stream error: {error}");
    };

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_output_stream(
            config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                for frame in data.chunks_mut(channels) {
                    let value = next_output_sample(&mut consumer, &gain, &muted);
                    for sample in frame {
                        *sample = value;
                    }
                }
            },
            error_callback,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_output_stream(
            config,
            move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                for frame in data.chunks_mut(channels) {
                    let value = next_output_sample(&mut consumer, &gain, &muted);
                    let quantized = (value.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
                    for sample in frame {
                        *sample = quantized;
                    }
                }
            },
            error_callback,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_output_stream(
            config,
            move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                for frame in data.chunks_mut(channels) {
                    let value = next_output_sample(&mut consumer, &gain, &muted);
                    let quantized = ((value.clamp(-1.0, 1.0) * 32_768.0) + 32_768.0) as u16;
                    for sample in frame {
                        *sample = quantized;
                    }
                }
            },
            error_callback,
            None,
        ),
        other => return Err(AudioIoError::UnsupportedSampleFormat(other)),
    };

    stream.map_err(|error| AudioIoError::Stream(error.to_string()))
}

/// Pops the next monitored sample (silence on underrun) and applies
/// gain/mute. Shared by every output sample-format branch so the
/// gain/mute/underrun behavior can't drift between them.
fn next_output_sample(
    consumer: &mut rtrb::Consumer<f32>,
    gain: &Arc<AtomicU32>,
    muted: &Arc<AtomicBool>,
) -> f32 {
    let raw = consumer.pop().unwrap_or(0.0);
    if muted.load(Ordering::Relaxed) {
        0.0
    } else {
        raw * f32::from_bits(gain.load(Ordering::Relaxed))
    }
}
