//! Capture and playback threads.
//!
//! ALSA has no callback model like CoreAudio's IOProc, so each stream owns a
//! thread that blocks in `snd_pcm_readi`/`snd_pcm_writei` one period at a time
//! and calls the backend-neutral callback with `f32` frames. A period is the
//! callback size, so `preferred_buffer_frames` maps onto it directly.
//!
//! Stopping sets a flag rather than calling into ALSA from another thread: the
//! loop notices within one period (2.7 ms at 128 frames and 48 kHz) and closes
//! the device itself, which keeps every libasound call on one thread.
//!
//! Xruns are recovered with `snd_pcm_recover` and counted; a device that has
//! gone away (`-ENODEV`) ends the thread and is reported through `failure`.
//! `snd_pcm_recover` reporting success does not mean the *next* read or write
//! will work: a PCM built on a broken slave (ALSA's `asym` type with an
//! undefined capture leg, seen on a real Pi 5 as a `pcm_asym.c: capture slave
//! is not defined` `default`) can recover-and-fail every single period with
//! no blocking in between, since the failing state itself makes the blocking
//! read/write return immediately. `run` bounds how long it will do that
//! before giving up, rather than spinning at the retry rate forever.

use std::ffi::c_int;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::backend::{Direction, InputCallback, OutputCallback, StreamFormat};
use crate::error::AudioIoError;

use super::pcm::{self, Configured, Pcm};

use super::{devices, ffi, DEFAULT_PERIOD_FRAMES};

/// How long `run` tolerates a read/write failing, recovering, and failing
/// again with no successful frame in between before it gives up on the
/// device. Generous next to one real period (tens of ms) so a genuine xrun
/// burst under system load has room to clear; short next to "forever", which
/// is what a PCM built on a permanently broken slave would otherwise cost.
const STALLED_RECOVERY_BUDGET: Duration = Duration::from_millis(500);

/// Whether continuous read/write failure-and-recovery with no successful
/// frame since `failing_since` has gone on long enough, as of `now`, to stop
/// retrying rather than keep going at whatever rate the hardware allows.
/// Pulled out of `run` as a pure function of its inputs so the threshold
/// itself is tested without needing a real (or fake) PCM.
fn recovery_has_stalled(failing_since: Instant, now: Instant, budget: Duration) -> bool {
    now.saturating_duration_since(failing_since) >= budget
}

pub(crate) struct Stream {
    stop: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
    xruns: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl Stream {
    pub(crate) fn failure(&self) -> Option<String> {
        self.failure.lock().ok().and_then(|failure| failure.clone())
    }

    /// Xruns are ALSA's equivalent of CoreAudio's overload notification.
    pub(crate) fn overloads(&self) -> u64 {
        self.xruns.load(Ordering::Relaxed)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

enum Callback {
    Input(InputCallback),
    Output(OutputCallback),
}

pub(crate) fn open_input(
    name: Option<&str>,
    preferred_buffer_frames: Option<u32>,
    callback: InputCallback,
) -> Result<(Stream, StreamFormat), AudioIoError> {
    open(
        Direction::Input,
        name,
        preferred_buffer_frames,
        Callback::Input(callback),
    )
}

pub(crate) fn open_output(
    name: Option<&str>,
    preferred_buffer_frames: Option<u32>,
    callback: OutputCallback,
) -> Result<(Stream, StreamFormat), AudioIoError> {
    open(
        Direction::Output,
        name,
        preferred_buffer_frames,
        Callback::Output(callback),
    )
}

fn open(
    direction: Direction,
    name: Option<&str>,
    preferred_buffer_frames: Option<u32>,
    callback: Callback,
) -> Result<(Stream, StreamFormat), AudioIoError> {
    let pcm_name = devices::resolve(direction, name)?;
    let display_name = devices::display_name(direction, &pcm_name);
    let period = preferred_buffer_frames.unwrap_or(DEFAULT_PERIOD_FRAMES);
    let stop = Arc::new(AtomicBool::new(false));
    let failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let xruns = Arc::new(AtomicU64::new(0));
    let (ready_tx, ready_rx) = mpsc::channel::<Result<Configured, AudioIoError>>();

    let thread_stop = stop.clone();
    let thread_failure = failure.clone();
    let thread_xruns = xruns.clone();
    let thread_name = pcm_name.clone();
    let capture = direction == Direction::Input;
    let thread = thread::Builder::new()
        .name(format!("loadngo-audio-io-alsa-{}", direction.label()))
        .spawn(move || {
            // Everything libasound touches stays on this thread.
            let opened = pcm::open_configured(&thread_name, capture, false, period);
            let (pcm, configured) = match opened {
                Ok(opened) => opened,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            if ready_tx.send(Ok(configured)).is_err() {
                pcm.close();
                return;
            }
            run(
                &pcm,
                configured,
                callback,
                &thread_stop,
                &thread_failure,
                &thread_xruns,
            );
            pcm.close();
        })
        .map_err(|error| AudioIoError::Thread(error.to_string()))?;

    let stream = Stream {
        stop,
        failure,
        xruns,
        thread: Some(thread),
    };
    match ready_rx.recv() {
        Ok(Ok(configured)) => Ok((
            stream,
            StreamFormat {
                device_name: display_name,
                sample_rate_hz: configured.sample_rate_hz,
                channels: configured.channels,
                buffer_frames: Some(configured.period_frames),
                resolution: Some(configured.format.resolution()),
            },
        )),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(AudioIoError::WorkerExited),
    }
}

fn run(
    pcm: &Pcm,
    configured: Configured,
    callback: Callback,
    stop: &AtomicBool,
    failure: &Mutex<Option<String>>,
    xruns: &AtomicU64,
) {
    let channels = usize::from(configured.channels.max(1));
    let frames = configured.period_frames.max(1) as usize;
    let mut bytes = vec![0u8; frames * channels * configured.format.bytes()];
    let mut samples: Vec<f32> = vec![0.0; frames * channels];
    let mut callback = callback;
    let mut failing_since: Option<Instant> = None;

    while !stop.load(Ordering::Relaxed) {
        let result = match &mut callback {
            Callback::Input(on_input) => {
                // SAFETY: `bytes` holds exactly `frames` periods of audio.
                let read = unsafe {
                    ffi::snd_pcm_readi(pcm.0, bytes.as_mut_ptr().cast(), frames as ffi::Uframes)
                };
                if read > 0 {
                    let taken = read as usize * channels * configured.format.bytes();
                    configured.format.decode(&bytes[..taken], &mut samples);
                    on_input(&samples, channels);
                }
                read
            }
            Callback::Output(on_output) => {
                samples.clear();
                samples.resize(frames * channels, 0.0);
                on_output(&mut samples, channels);
                configured.format.encode(&samples, &mut bytes);
                // SAFETY: `bytes` now holds exactly `frames` frames.
                unsafe { ffi::snd_pcm_writei(pcm.0, bytes.as_ptr().cast(), frames as ffi::Uframes) }
            }
        };

        if result >= 0 {
            failing_since = None;
            continue;
        }
        let code = result as c_int;
        if code == -ffi::EPIPE || code == -ffi::ESTRPIPE {
            xruns.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: `pcm` is this thread's open handle.
        let recovered = unsafe { ffi::snd_pcm_recover(pcm.0, code, 1) };
        let since = *failing_since.get_or_insert_with(Instant::now);
        let stalled =
            recovered >= 0 && recovery_has_stalled(since, Instant::now(), STALLED_RECOVERY_BUDGET);
        if recovered < 0 || stalled {
            let reason = if code == -ffi::ENODEV {
                "the audio device was disconnected".to_string()
            } else if stalled {
                format!(
                    "audio I/O kept failing and recovering with no data for over {}ms: {}",
                    STALLED_RECOVERY_BUDGET.as_millis(),
                    ffi::error_text(code)
                )
            } else {
                format!("audio I/O failed: {}", ffi::error_text(code))
            };
            if let Ok(mut failure) = failure.lock() {
                *failure = Some(reason);
            }
            return;
        }
    }
    // SAFETY: stopping from the thread that owns the handle.
    unsafe { ffi::snd_pcm_drop(pcm.0) };
}

#[cfg(test)]
mod tests {
    use super::{recovery_has_stalled, STALLED_RECOVERY_BUDGET};
    use std::time::{Duration, Instant};

    // The bug this guards: a PCM built on a broken slave (ALSA's `asym`
    // type with an undefined capture leg) fails every readi/writei
    // immediately while `snd_pcm_recover` keeps reporting success, so
    // nothing here can be exercised without real (or faked) libasound
    // calls except the one piece that actually decides when to stop: how
    // long "failing and recovering with no data" has to go on. These test
    // that threshold directly, the same way `pcm.rs`'s tests cover format
    // conversion without opening a real device.

    #[test]
    fn does_not_stall_before_the_budget_elapses() {
        let failing_since = Instant::now();
        let almost_there = failing_since + STALLED_RECOVERY_BUDGET - Duration::from_millis(1);
        assert!(!recovery_has_stalled(
            failing_since,
            almost_there,
            STALLED_RECOVERY_BUDGET
        ));
    }

    #[test]
    fn stalls_the_instant_the_budget_is_reached() {
        let failing_since = Instant::now();
        let at_budget = failing_since + STALLED_RECOVERY_BUDGET;
        assert!(recovery_has_stalled(
            failing_since,
            at_budget,
            STALLED_RECOVERY_BUDGET
        ));
    }

    #[test]
    fn stays_stalled_well_past_the_budget() {
        let failing_since = Instant::now();
        let long_after = failing_since + STALLED_RECOVERY_BUDGET * 10;
        assert!(recovery_has_stalled(
            failing_since,
            long_after,
            STALLED_RECOVERY_BUDGET
        ));
    }

    #[test]
    fn a_single_instant_with_zero_elapsed_time_has_not_stalled() {
        // The first failure in a streak: `failing_since` was just set to
        // `now` (see `run`'s `get_or_insert_with(Instant::now)`), so this
        // must never immediately read as stalled on the very first retry.
        let now = Instant::now();
        assert!(!recovery_has_stalled(now, now, STALLED_RECOVERY_BUDGET));
    }
}
