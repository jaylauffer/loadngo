//! What an input device can capture.
//!
//! ALSA's `hw:` parameters *are* the hardware's: unlike CoreAudio, there is no
//! separate virtual format hiding the converter, so a device that reports
//! `S16_LE` at 48 kHz really is 16-bit at 48 kHz. That means `current_physical`
//! and `current_stream` describe the same thing here, and there is nothing to
//! switch -- the format is chosen when the stream is opened, so
//! `set_input_physical_format` has no meaning on this platform.

use crate::backend::{Direction, StreamFormat};
use crate::capabilities::{
    CurrentFormat, FormatRange, InputCapabilities, STANDARD_SAMPLE_RATES_HZ,
};
use crate::error::AudioIoError;

use super::pcm::{self, HwParams};
use super::{devices, DEFAULT_PERIOD_FRAMES};

/// Opens the device briefly to read its parameters, then closes it.
pub(crate) fn describe(
    direction: Direction,
    name: Option<&str>,
) -> Result<StreamFormat, AudioIoError> {
    let pcm_name = devices::resolve(direction, name)?;
    let (pcm, configured) = pcm::open_configured(
        &pcm_name,
        direction == Direction::Input,
        true,
        DEFAULT_PERIOD_FRAMES,
    )?;
    pcm.close();
    Ok(StreamFormat {
        device_name: devices::display_name(direction, &pcm_name),
        sample_rate_hz: configured.sample_rate_hz,
        channels: configured.channels,
        buffer_frames: Some(configured.period_frames),
        resolution: Some(configured.format.resolution()),
    })
}

pub(crate) fn probe_input_capabilities(
    device_name: Option<&str>,
) -> Result<InputCapabilities, AudioIoError> {
    let pcm_name = devices::resolve(Direction::Input, device_name)?;
    let (pcm, configured) = pcm::open_configured(&pcm_name, true, true, DEFAULT_PERIOD_FRAMES)?;
    let probed = HwParams::any(&pcm).map(|params| {
        let formats = params.supported_formats(&pcm);
        let (_, channels) = params.channel_range();
        let rates: Vec<u32> = STANDARD_SAMPLE_RATES_HZ
            .into_iter()
            .filter(|rate| params.supports_rate(&pcm, *rate))
            .collect();
        let mut ranges = Vec::new();
        for format in &formats {
            for rate in &rates {
                ranges.push(FormatRange {
                    channels,
                    min_sample_rate_hz: *rate,
                    max_sample_rate_hz: *rate,
                    resolution: format.resolution(),
                });
            }
        }
        ranges
    });
    pcm.close();
    let ranges = probed?;
    let current = CurrentFormat {
        sample_rate_hz: configured.sample_rate_hz,
        channels: configured.channels,
        resolution: configured.format.resolution(),
    };
    let devices = devices::enumerate(Direction::Input);
    let entry = devices.iter().find(|(_, resolved)| *resolved == pcm_name);
    Ok(InputCapabilities {
        device_name: entry.map_or_else(|| pcm_name.clone(), |(info, _)| info.name.clone()),
        is_default: entry.is_some_and(|(info, _)| info.is_default),
        current_stream: current,
        // On ALSA these are the hardware's own parameters, so the physical
        // format is the stream format rather than something behind it.
        current_physical: Some(current),
        physical_formats: ranges.clone(),
        stream_formats: ranges,
    })
}

pub(crate) fn set_input_physical_format(
    _device_name: &str,
    _format: &CurrentFormat,
) -> Result<(), AudioIoError> {
    Err(AudioIoError::UnsupportedOnPlatform(
        "changing a converter's physical format (ALSA picks the hardware format when a stream opens)",
    ))
}
