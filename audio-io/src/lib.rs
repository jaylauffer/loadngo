//! Foundational live audio-input tooling for `loadngo` games and tools.
//!
//! [`pitch`] is platform-agnostic monophonic pitch detection and note
//! naming -- the shared math behind a chromatic instrument tuner. On
//! desktop targets (macOS/Linux/Windows), this crate additionally exposes
//! device enumeration ([`list_input_devices`], [`list_output_devices`]) and
//! [`LiveMonitor`], a low-latency input-to-output passthrough for turning a
//! line-in signal (e.g. a USB instrument interface) into live playback on a
//! selected output device, with a `drain_tap` hook so a tuner and the
//! monitor share one open input stream instead of each opening their own --
//! see `loadngo/docs/AUDIO_IO.md`.

pub mod pitch;

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
mod devices;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
mod error;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
mod monitor;

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub use devices::{list_input_devices, list_output_devices, AudioDeviceInfo};
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub use error::AudioIoError;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub use monitor::{LiveMonitor, LiveMonitorConfig};

pub use pitch::{
    closest_bass_string, nearest_note, NoteReading, PitchDetector, PitchEstimate,
    BASS_STANDARD_TUNING,
};
