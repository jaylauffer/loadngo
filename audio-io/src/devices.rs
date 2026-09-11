//! Live audio device enumeration, for a settings UI to let a player pick
//! which interface to listen through and which output to play back on.

use crate::backend::{platform, Direction};
use crate::error::AudioIoError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDeviceInfo {
    pub name: String,
    pub is_default: bool,
}

/// Every input device the current host reports (e.g. a USB instrument
/// interface's line/mic input alongside the system default), with the
/// current default flagged.
pub fn list_input_devices() -> Result<Vec<AudioDeviceInfo>, AudioIoError> {
    platform::list_devices(Direction::Input)
}

/// Every output device the current host reports (e.g. Mac mini speakers,
/// a connected USB interface's output, headphones), with the current
/// default flagged -- including output-only devices, which `cpal` 0.15 missed
/// on macOS.
pub fn list_output_devices() -> Result<Vec<AudioDeviceInfo>, AudioIoError> {
    platform::list_devices(Direction::Output)
}
