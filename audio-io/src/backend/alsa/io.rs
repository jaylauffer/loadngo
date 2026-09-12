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

use std::ffi::c_int;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::backend::{Direction, InputCallback, OutputCallback, StreamFormat};
use crate::error::AudioIoError;

use super::pcm::{self, Configured, Pcm};

use super::{devices, ffi, DEFAULT_PERIOD_FRAMES};

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
            continue;
        }
        let code = result as c_int;
        if code == -ffi::EPIPE || code == -ffi::ESTRPIPE {
            xruns.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: `pcm` is this thread's open handle.
        let recovered = unsafe { ffi::snd_pcm_recover(pcm.0, code, 1) };
        if recovered < 0 {
            let reason = if code == -ffi::ENODEV {
                "the audio device was disconnected".to_string()
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
