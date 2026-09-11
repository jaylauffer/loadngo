//! What a converter can capture, and switching it.
//!
//! Two formats matter and the HAL keeps them apart:
//! - **virtual** (`kAudioStreamProperty[Available]VirtualFormat[s]`) -- what
//!   this process receives, always 32-bit float for a shared device;
//! - **physical** (`kAudioStreamProperty[Available]PhysicalFormat[s]`) --
//!   what the converter itself runs at, e.g. 24-bit integer.
//!
//! See `docs/AUDIO_IO.md` ("Converter capabilities") for what probing this
//! Mac mini's interfaces found, including a 24-bit converter running at 16.

use coreaudio_sys::{
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsSignedInteger, kAudioFormatLinearPCM,
    kAudioObjectPropertyScopeGlobal, kAudioStreamPropertyAvailablePhysicalFormats,
    kAudioStreamPropertyAvailableVirtualFormats, kAudioStreamPropertyPhysicalFormat, AudioObjectID,
    AudioStreamBasicDescription, AudioStreamRangedDescription,
};

use crate::backend::Direction;
use crate::capabilities::{
    CurrentFormat, FormatRange, InputCapabilities, SampleEncoding, SampleResolution,
};
use crate::error::AudioIoError;

use super::{devices, hal};

pub(crate) fn probe_input_capabilities(
    device_name: Option<&str>,
) -> Result<InputCapabilities, AudioIoError> {
    let device = devices::resolve(Direction::Input, device_name)?;
    let stream = devices::stream_format(device, Direction::Input)?;
    let streams = hal::streams(device, Direction::Input);

    let mut current_physical: Option<CurrentFormat> = None;
    let mut physical_ranges = Vec::new();
    let mut stream_ranges = Vec::new();
    for stream_id in &streams {
        if let Some(format) = hal::property_value::<AudioStreamBasicDescription>(
            *stream_id,
            kAudioStreamPropertyPhysicalFormat,
            kAudioObjectPropertyScopeGlobal,
        )
        .and_then(|asbd| current_format(&asbd))
        {
            current_physical = Some(match current_physical {
                // A device with several input streams (e.g. two stereo
                // pairs) captures all of them; report the total width.
                Some(existing) => CurrentFormat {
                    channels: existing.channels.saturating_add(format.channels),
                    ..existing
                },
                None => format,
            });
        }
        physical_ranges.extend(ranged(
            *stream_id,
            kAudioStreamPropertyAvailablePhysicalFormats,
        ));
        stream_ranges.extend(ranged(
            *stream_id,
            kAudioStreamPropertyAvailableVirtualFormats,
        ));
    }

    Ok(InputCapabilities {
        is_default: hal::default_device(Direction::Input) == Some(device),
        device_name: stream.device_name,
        current_stream: CurrentFormat {
            sample_rate_hz: stream.sample_rate_hz,
            channels: stream.channels,
            resolution: SampleResolution::new(32, SampleEncoding::Float),
        },
        stream_formats: unique_sorted(stream_ranges),
        current_physical,
        physical_formats: unique_sorted(physical_ranges),
    })
}

/// Switches every input stream of the named device to the physical format
/// matching `format` -- the same system-wide setting as Audio MIDI Setup's
/// "Format" menu, so it persists after this app exits and affects every
/// other app using the device. It applies asynchronously.
pub(crate) fn set_input_physical_format(
    device_name: &str,
    format: &CurrentFormat,
) -> Result<(), AudioIoError> {
    let device = devices::resolve(Direction::Input, Some(device_name))?;
    for stream in hal::streams(device, Direction::Input) {
        let available: Vec<AudioStreamRangedDescription> = hal::property_array(
            stream,
            kAudioStreamPropertyAvailablePhysicalFormats,
            kAudioObjectPropertyScopeGlobal,
        );
        let Some(ranged) = available.iter().find(|ranged| {
            format_range(ranged).is_some_and(|range| {
                range.resolution == format.resolution && range.contains_rate(format.sample_rate_hz)
            })
        }) else {
            return Err(AudioIoError::UnsupportedPhysicalFormat(format!(
                "{} at {} Hz",
                format.resolution, format.sample_rate_hz
            )));
        };
        let mut asbd = ranged.mFormat;
        asbd.mSampleRate = f64::from(format.sample_rate_hz);
        hal::set_property_value(
            stream,
            kAudioStreamPropertyPhysicalFormat,
            kAudioObjectPropertyScopeGlobal,
            &asbd,
        )
        .map_err(|status| {
            AudioIoError::Backend(format!(
                "CoreAudio refused the physical format change (OSStatus {status})"
            ))
        })?;
    }
    Ok(())
}

fn ranged(stream: AudioObjectID, selector: u32) -> Vec<FormatRange> {
    hal::property_array::<AudioStreamRangedDescription>(
        stream,
        selector,
        kAudioObjectPropertyScopeGlobal,
    )
    .iter()
    .filter_map(format_range)
    .collect()
}

/// Multi-stream devices advertise the same list once per stream.
fn unique_sorted(ranges: Vec<FormatRange>) -> Vec<FormatRange> {
    let mut unique: Vec<FormatRange> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if !unique.contains(&range) {
            unique.push(range);
        }
    }
    unique.sort_by(|a, b| {
        (b.resolution.bits, a.min_sample_rate_hz, a.channels).cmp(&(
            a.resolution.bits,
            b.min_sample_rate_hz,
            b.channels,
        ))
    });
    unique
}

fn resolution(asbd: &AudioStreamBasicDescription) -> Option<SampleResolution> {
    if asbd.mFormatID != kAudioFormatLinearPCM {
        return None;
    }
    let encoding = if asbd.mFormatFlags & kAudioFormatFlagIsFloat != 0 {
        SampleEncoding::Float
    } else if asbd.mFormatFlags & kAudioFormatFlagIsSignedInteger != 0 {
        SampleEncoding::SignedInt
    } else {
        SampleEncoding::UnsignedInt
    };
    Some(SampleResolution::new(
        u16::try_from(asbd.mBitsPerChannel).ok()?,
        encoding,
    ))
}

fn current_format(asbd: &AudioStreamBasicDescription) -> Option<CurrentFormat> {
    Some(CurrentFormat {
        sample_rate_hz: hal::rate_hz(asbd.mSampleRate),
        channels: u16::try_from(asbd.mChannelsPerFrame).ok()?,
        resolution: resolution(asbd)?,
    })
}

fn format_range(ranged: &AudioStreamRangedDescription) -> Option<FormatRange> {
    Some(FormatRange {
        channels: u16::try_from(ranged.mFormat.mChannelsPerFrame).ok()?,
        min_sample_rate_hz: hal::rate_hz(ranged.mSampleRateRange.mMinimum),
        max_sample_rate_hz: hal::rate_hz(ranged.mSampleRateRange.mMaximum),
        resolution: resolution(&ranged.mFormat)?,
    })
}
