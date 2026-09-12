//! Opening an output device and feeding it, without the rest of the crate.
//!
//! [`LiveMonitor`](crate::LiveMonitor) owns the whole duplex path: an input
//! device, drift correction between two clocks, and the analysis and
//! recording taps. A game host wants only the other half -- somewhere to push
//! already-mixed samples -- and has no input device at all.
//!
//! This exposes exactly that half, over the same platform backends the
//! monitor uses (CoreAudio on macOS, ALSA on Linux, `cpal` on Windows until
//! WASAPI lands). The point is that playback and capture share one audio
//! stack: a second, parallel one would double the places a device-death or
//! format bug can hide, and those are precisely the bugs that cost the most
//! to find. See `loadngo/docs/AUDIO_BACKENDS.md`.

use crate::backend::{platform, OutputCallback};
use crate::error::AudioIoError;

/// What the device actually opened at, which is not always what was asked
/// for -- mix to this rather than to an assumption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFormat {
    pub device_name: String,
    pub sample_rate_hz: u32,
    pub channels: u16,
    /// Frames per callback, when the backend knows it ahead of time.
    pub buffer_frames: Option<u32>,
}

/// A running output device. Dropping it stops the device.
pub struct OutputStream {
    /// Deliberately private: the backend handle is crate-internal, and
    /// keeping it so means the public API never leaks a platform type.
    stream: platform::Stream,
    format: OutputFormat,
}

impl OutputStream {
    #[must_use]
    pub fn format(&self) -> &OutputFormat {
        &self.format
    }

    #[must_use]
    pub fn sample_rate_hz(&self) -> u32 {
        self.format.sample_rate_hz
    }

    #[must_use]
    pub fn channels(&self) -> u16 {
        self.format.channels
    }

    /// `Some(reason)` once the device has gone away or the callback failed.
    ///
    /// Worth polling: an unplugged interface is silent either way, but a
    /// caller that never asks cannot tell that apart from a mix that has
    /// stopped producing sound, which is the harder bug.
    #[must_use]
    pub fn failure(&self) -> Option<String> {
        self.stream.failure()
    }

    /// How many times the OS reported the callback missed its deadline.
    #[must_use]
    pub fn overloads(&self) -> u64 {
        self.stream.overloads()
    }
}

/// Opens `device_name` (or the host's default when `None`) and calls `fill`
/// whenever the device wants more samples.
///
/// `fill` receives an interleaved buffer and the channel count, and must
/// write **every** sample: what it leaves behind is whatever the buffer held
/// before, which is usually the previous block, not silence.
///
/// It runs on a real-time thread. Don't allocate in it, don't do I/O in it,
/// and don't take a lock that a slower thread can hold -- prefer `try_lock`
/// and emit silence on contention, because one silent buffer is far cheaper
/// than blocking the audio thread.
pub fn open_output_stream(
    device_name: Option<&str>,
    preferred_buffer_frames: Option<u32>,
    fill: impl FnMut(&mut [f32], usize) + Send + 'static,
) -> Result<OutputStream, AudioIoError> {
    let callback: OutputCallback = Box::new(fill);
    let (stream, format) = platform::open_output(device_name, preferred_buffer_frames, callback)?;
    Ok(OutputStream {
        stream,
        format: OutputFormat {
            device_name: format.device_name,
            sample_rate_hz: format.sample_rate_hz,
            channels: format.channels,
            buffer_frames: format.buffer_frames,
        },
    })
}
