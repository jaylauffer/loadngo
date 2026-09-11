//! macOS-only: reads an input device's *physical* (converter) formats from
//! CoreAudio, which `cpal` does not expose -- its CoreAudio backend only
//! reports the HAL's 32-bit float virtual format. See `capabilities.rs`'s
//! module doc for why the distinction matters.
//!
//! Devices are matched by name because `cpal::Device` does not expose its
//! `AudioObjectID`. The name lookup mirrors `cpal`'s own
//! (`kAudioDevicePropertyDeviceNameCFString`, UTF-8), so any device `cpal`
//! can name, this can find.

use std::ffi::{c_char, c_void, CStr};
use std::mem;
use std::ptr;

use coreaudio_sys::{
    kAudioDevicePropertyDeviceNameCFString, kAudioDevicePropertyStreams, kAudioFormatFlagIsFloat,
    kAudioFormatFlagIsSignedInteger, kAudioFormatLinearPCM, kAudioHardwareNoError,
    kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMaster,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput, kAudioObjectSystemObject,
    kAudioStreamPropertyAvailablePhysicalFormats, kAudioStreamPropertyPhysicalFormat,
    kCFStringEncodingUTF8, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
    AudioObjectID, AudioObjectPropertyAddress, AudioObjectSetPropertyData,
    AudioStreamBasicDescription, AudioStreamRangedDescription, CFRelease, CFStringGetCString,
    CFStringRef,
};

use crate::capabilities::{CurrentFormat, FormatRange, SampleEncoding, SampleResolution};
use crate::error::AudioIoError;

/// The named input device's current physical format (summed across its
/// input streams) and every physical format its streams advertise. Returns
/// `(None, [])` if the device can't be found or reports nothing -- physical
/// formats are extra detail, not something a caller should fail over.
pub(crate) fn physical_input_formats(
    device_name: &str,
) -> (Option<CurrentFormat>, Vec<FormatRange>) {
    let streams = input_streams(device_name);

    let mut current: Option<CurrentFormat> = None;
    let mut ranges = Vec::new();
    for stream in streams {
        if let Some(format) = property_value::<AudioStreamBasicDescription>(
            stream,
            kAudioStreamPropertyPhysicalFormat,
            kAudioObjectPropertyScopeGlobal,
        )
        .and_then(|asbd| current_format(&asbd))
        {
            current = Some(match current {
                // A device with several input streams (e.g. two stereo
                // pairs) captures all of them; report the total width.
                Some(existing) => CurrentFormat {
                    channels: existing.channels.saturating_add(format.channels),
                    ..existing
                },
                None => format,
            });
        }

        let available: Vec<AudioStreamRangedDescription> = property_array(
            stream,
            kAudioStreamPropertyAvailablePhysicalFormats,
            kAudioObjectPropertyScopeGlobal,
        );
        ranges.extend(available.iter().filter_map(format_range));
    }

    // Multi-stream devices advertise the same list once per stream.
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
    (current, unique)
}

/// Switches every input stream of the named device to the physical format
/// matching `format` -- the same system-wide setting as Audio MIDI Setup's
/// "Format" menu, so it persists after this app exits and affects every
/// other app using the device. The OS stream delivered to apps stays 32-bit
/// float either way; only the converter's own resolution/rate changes.
pub(crate) fn set_physical_input_format(
    device_name: &str,
    format: &CurrentFormat,
) -> Result<(), AudioIoError> {
    let streams = input_streams(device_name);
    if streams.is_empty() {
        return Err(AudioIoError::InputDeviceNotFound(device_name.to_string()));
    }
    for stream in streams {
        let available: Vec<AudioStreamRangedDescription> = property_array(
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
        let address = address(
            kAudioStreamPropertyPhysicalFormat,
            kAudioObjectPropertyScopeGlobal,
        );
        let size = u32::try_from(mem::size_of::<AudioStreamBasicDescription>())
            .map_err(|error| AudioIoError::Cpal(error.to_string()))?;
        // SAFETY: `asbd` is a fully initialized ASBD copied from CoreAudio's
        // own advertised list, and `size` is exactly its size.
        let status = unsafe {
            AudioObjectSetPropertyData(
                stream,
                &address,
                0,
                ptr::null(),
                size,
                ptr::addr_of!(asbd).cast::<c_void>(),
            )
        };
        if status != kAudioHardwareNoError as i32 {
            return Err(AudioIoError::Cpal(format!(
                "CoreAudio refused the physical format change (OSStatus {status})"
            )));
        }
    }
    Ok(())
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

// Real nominal rates are whole numbers far below u32::MAX.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn rate_hz(rate: f64) -> u32 {
    rate.round() as u32
}

fn current_format(asbd: &AudioStreamBasicDescription) -> Option<CurrentFormat> {
    Some(CurrentFormat {
        sample_rate_hz: rate_hz(asbd.mSampleRate),
        channels: u16::try_from(asbd.mChannelsPerFrame).ok()?,
        resolution: resolution(asbd)?,
    })
}

fn format_range(ranged: &AudioStreamRangedDescription) -> Option<FormatRange> {
    Some(FormatRange {
        channels: u16::try_from(ranged.mFormat.mChannelsPerFrame).ok()?,
        min_sample_rate_hz: rate_hz(ranged.mSampleRateRange.mMinimum),
        max_sample_rate_hz: rate_hz(ranged.mSampleRateRange.mMaximum),
        resolution: resolution(&ranged.mFormat)?,
    })
}

/// The input streams of the device named `name`. A USB interface with both
/// directions can appear as *two* CoreAudio devices sharing one name (seen
/// on "USB PnP Audio Device", whose output object enumerates first), so this
/// skips same-named devices that have no input streams rather than stopping
/// at the first name match.
fn input_streams(name: &str) -> Vec<AudioObjectID> {
    let devices: Vec<AudioObjectID> = property_array(
        kAudioObjectSystemObject,
        kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
    );
    devices
        .into_iter()
        .filter(|device| device_name(*device).as_deref() == Some(name))
        .map(|device| {
            property_array::<AudioObjectID>(
                device,
                kAudioDevicePropertyStreams,
                kAudioObjectPropertyScopeInput,
            )
        })
        .find(|streams| !streams.is_empty())
        .unwrap_or_default()
}

fn device_name(device: AudioObjectID) -> Option<String> {
    let cf_name = property_value::<CFStringRef>(
        device,
        kAudioDevicePropertyDeviceNameCFString,
        kAudioObjectPropertyScopeGlobal,
    )?;
    if cf_name.is_null() {
        return None;
    }
    let mut buffer = [0 as c_char; 512];
    // SAFETY: `cf_name` is a valid CFString CoreAudio just handed us, and
    // `buffer` is writable for its full stated length. The property follows
    // the CoreFoundation "copy" rule, so we own the reference and release
    // it exactly once below.
    let name = unsafe {
        let ok = CFStringGetCString(
            cf_name,
            buffer.as_mut_ptr(),
            buffer.len() as _,
            kCFStringEncodingUTF8,
        );
        CFRelease(cf_name.cast());
        if ok == 0 {
            return None;
        }
        CStr::from_ptr(buffer.as_ptr())
    };
    name.to_str().ok().map(str::to_owned)
}

fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMaster,
    }
}

/// Reads a fixed-size property. `T` must be the plain-data type CoreAudio
/// documents for `selector`.
fn property_value<T: Copy + Default>(
    object: AudioObjectID,
    selector: u32,
    scope: u32,
) -> Option<T> {
    let address = address(selector, scope);
    let mut value = T::default();
    let mut size = u32::try_from(mem::size_of::<T>()).ok()?;
    // SAFETY: `value` is a live, writable `T` of exactly `size` bytes, and
    // CoreAudio writes at most `size` bytes into it.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &address,
            0,
            ptr::null(),
            &mut size,
            ptr::addr_of_mut!(value).cast::<c_void>(),
        )
    };
    (status == kAudioHardwareNoError as i32).then_some(value)
}

/// Reads a variable-length array property. `T` must be the plain-data
/// element type CoreAudio documents for `selector`.
fn property_array<T: Copy>(object: AudioObjectID, selector: u32, scope: u32) -> Vec<T> {
    let address = address(selector, scope);
    let mut size = 0u32;
    // SAFETY: querying a size writes only into `size`.
    let status =
        unsafe { AudioObjectGetPropertyDataSize(object, &address, 0, ptr::null(), &mut size) };
    let element_size = mem::size_of::<T>();
    if status != kAudioHardwareNoError as i32 || element_size == 0 {
        return Vec::new();
    }
    let capacity = size as usize / element_size;
    let mut values: Vec<T> = Vec::with_capacity(capacity);
    let mut filled = u32::try_from(capacity * element_size).unwrap_or(0);
    // SAFETY: `values` has room for `capacity` elements (`filled` bytes, a
    // whole multiple of `T`'s size, so alignment holds); CoreAudio writes at
    // most `filled` bytes and reports how many it wrote. We only expose the
    // fully written elements via `set_len`.
    unsafe {
        let status = AudioObjectGetPropertyData(
            object,
            &address,
            0,
            ptr::null(),
            &mut filled,
            values.as_mut_ptr().cast::<c_void>(),
        );
        if status != kAudioHardwareNoError as i32 {
            return Vec::new();
        }
        values.set_len((filled as usize / element_size).min(capacity));
    }
    values
}
