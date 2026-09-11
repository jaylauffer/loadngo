//! Device enumeration and resolution, straight from the HAL.
//!
//! Every device with streams in a direction is listed -- including
//! output-only devices (built-in speakers, HDMI) that `cpal` 0.15 dropped.
//! One physical interface can be several CoreAudio objects sharing a name
//! (a USB interface's input and output halves are separate devices), so
//! names are resolved per direction: the first same-named object that has
//! streams that way.

use coreaudio_sys::{kAudioFormatFlagIsFloat, kAudioFormatLinearPCM, AudioObjectID};

use crate::backend::{Direction, StreamFormat};
use crate::capabilities::{SampleEncoding, SampleResolution};
use crate::devices::AudioDeviceInfo;
use crate::error::AudioIoError;

use super::hal;

pub(crate) fn list_devices(direction: Direction) -> Result<Vec<AudioDeviceInfo>, AudioIoError> {
    let default = hal::default_device(direction);
    let mut devices: Vec<AudioDeviceInfo> = Vec::new();
    for device in hal::all_devices() {
        if hal::streams(device, direction).is_empty() {
            continue;
        }
        let Some(name) = hal::device_name(device) else {
            continue;
        };
        let is_default = default == Some(device);
        match devices.iter_mut().find(|existing| existing.name == name) {
            Some(existing) => existing.is_default |= is_default,
            None => devices.push(AudioDeviceInfo { name, is_default }),
        }
    }
    Ok(devices)
}

/// The device `name` names in `direction`, or the system default for `None`.
pub(crate) fn resolve(
    direction: Direction,
    name: Option<&str>,
) -> Result<AudioObjectID, AudioIoError> {
    match name {
        Some(name) => hal::all_devices()
            .into_iter()
            .find(|device| {
                hal::device_name(*device).as_deref() == Some(name)
                    && !hal::streams(*device, direction).is_empty()
            })
            .ok_or_else(|| match direction {
                Direction::Input => AudioIoError::InputDeviceNotFound(name.to_string()),
                Direction::Output => AudioIoError::OutputDeviceNotFound(name.to_string()),
            }),
        None => hal::default_device(direction).ok_or(match direction {
            Direction::Input => AudioIoError::NoDefaultInputDevice,
            Direction::Output => AudioIoError::NoDefaultOutputDevice,
        }),
    }
}

/// What a stream opened on `device` in `direction` delivers: the nominal
/// rate, every channel across its streams, as 32-bit float. Errors if a
/// stream's virtual format isn't linear-PCM float, which the IO proc relies
/// on (the HAL's mixable format always is; exclusive "hog mode" formats
/// are the exception).
pub(crate) fn stream_format(
    device: AudioObjectID,
    direction: Direction,
) -> Result<StreamFormat, AudioIoError> {
    let device_name = hal::device_name(device).unwrap_or_else(|| format!("device {device}"));
    let formats = hal::virtual_formats(device, direction);
    if formats.is_empty() {
        return Err(AudioIoError::Backend(format!(
            "{device_name} has no {} streams",
            direction.label()
        )));
    }
    let mut channels: u32 = 0;
    for format in &formats {
        let float = format.mFormatFlags & kAudioFormatFlagIsFloat != 0;
        if format.mFormatID != kAudioFormatLinearPCM || !float || format.mBitsPerChannel != 32 {
            return Err(AudioIoError::UnsupportedSampleFormat(format!(
                "{device_name}: {}-bit, flags {:#x} (need 32-bit float)",
                format.mBitsPerChannel, format.mFormatFlags
            )));
        }
        // A non-interleaved stream (`kAudioFormatFlagIsNonInterleaved`)
        // arrives as one buffer per channel, which the IO proc handles the
        // same way as several mono streams, so it needs no special case.
        channels += format.mChannelsPerFrame;
    }
    let sample_rate_hz =
        hal::nominal_sample_rate(device).unwrap_or_else(|| hal::rate_hz(formats[0].mSampleRate));
    Ok(StreamFormat {
        device_name,
        sample_rate_hz,
        channels: u16::try_from(channels).unwrap_or(u16::MAX),
        buffer_frames: hal::buffer_frame_size(device),
        resolution: Some(SampleResolution::new(32, SampleEncoding::Float)),
    })
}

pub(crate) fn describe(
    direction: Direction,
    name: Option<&str>,
) -> Result<StreamFormat, AudioIoError> {
    stream_format(resolve(direction, name)?, direction)
}
