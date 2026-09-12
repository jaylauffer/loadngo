//! The seam between backend-neutral audio code (`LiveMonitor`, the taps,
//! `DriftResampler`) and each platform's device I/O. One backend per build,
//! chosen by `cfg`, exposed as `platform`: CoreAudio on macOS, ALSA on Linux,
//! `cpal` on Windows until a WASAPI backend lands. See
//! `loadngo/docs/AUDIO_BACKENDS.md`.
//!
//! Every backend provides the same functions with the same signatures:
//! `list_devices`, `describe`, `open_input`, `open_output`,
//! `probe_input_capabilities`, `set_input_physical_format`, and a `Stream`
//! handle with `failure()` and `overloads()` that stops I/O when dropped.

#[cfg(target_os = "macos")]
pub(crate) mod coreaudio;
#[cfg(target_os = "macos")]
pub(crate) use coreaudio as platform;

#[cfg(target_os = "linux")]
pub(crate) mod alsa;
#[cfg(target_os = "linux")]
pub(crate) use alsa as platform;

#[cfg(target_os = "windows")]
pub(crate) mod cpal_host;
#[cfg(target_os = "windows")]
pub(crate) use cpal_host as platform;

use crate::capabilities::SampleResolution;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Input,
    Output,
}

impl Direction {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
        }
    }
}

/// Called on the audio thread with interleaved samples and their channel
/// count. Must not block or allocate.
pub(crate) type InputCallback = Box<dyn FnMut(&[f32], usize) + Send + 'static>;
/// Called on the audio thread to fill interleaved samples for the given
/// channel count. Must not block or allocate.
pub(crate) type OutputCallback = Box<dyn FnMut(&mut [f32], usize) + Send + 'static>;

/// What a stream on a device delivers or accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StreamFormat {
    pub(crate) device_name: String,
    pub(crate) sample_rate_hz: u32,
    pub(crate) channels: u16,
    /// Frames per callback, when the backend knows it ahead of time.
    pub(crate) buffer_frames: Option<u32>,
    /// The encoding the OS delivers, when it's meaningful to report.
    pub(crate) resolution: Option<SampleResolution>,
}
