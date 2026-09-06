//! Live audio device enumeration, for a settings UI to let a player pick
//! which interface to listen through and which output to play back on.

use cpal::traits::{DeviceTrait, HostTrait};

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
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|device| device.name().ok());
    let devices = host
        .input_devices()
        .map_err(|error| AudioIoError::Cpal(error.to_string()))?;
    Ok(devices
        .filter_map(|device| device.name().ok())
        .map(|name| {
            let is_default = default_name.as_deref() == Some(name.as_str());
            AudioDeviceInfo { name, is_default }
        })
        .collect())
}

/// Every output device the current host reports (e.g. Mac mini speakers,
/// a connected USB interface's output, headphones), with the current
/// default flagged.
pub fn list_output_devices() -> Result<Vec<AudioDeviceInfo>, AudioIoError> {
    let host = cpal::default_host();
    let default_name = host
        .default_output_device()
        .and_then(|device| device.name().ok());
    let devices = host
        .output_devices()
        .map_err(|error| AudioIoError::Cpal(error.to_string()))?;
    Ok(devices
        .filter_map(|device| device.name().ok())
        .map(|name| {
            let is_default = default_name.as_deref() == Some(name.as_str());
            AudioDeviceInfo { name, is_default }
        })
        .collect())
}
