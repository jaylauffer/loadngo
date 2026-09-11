//! Typed reads and writes of CoreAudio HAL properties, and the few object
//! queries every other file in this backend builds on. All `unsafe` property
//! access in the backend goes through the three functions at the bottom.

use std::ffi::{c_char, c_void, CStr};
use std::mem;
use std::ptr;

use coreaudio_sys::{
    kAudioDevicePropertyBufferFrameSize, kAudioDevicePropertyBufferFrameSizeRange,
    kAudioDevicePropertyDeviceNameCFString, kAudioDevicePropertyNominalSampleRate,
    kAudioDevicePropertyStreams, kAudioHardwareNoError, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyDevices,
    kAudioObjectPropertyElementMaster, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
    kAudioStreamPropertyVirtualFormat, kCFStringEncodingUTF8, AudioObjectGetPropertyData,
    AudioObjectGetPropertyDataSize, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectSetPropertyData, AudioStreamBasicDescription, AudioValueRange, CFRelease,
    CFStringGetCString, CFStringRef, OSStatus,
};

use crate::backend::Direction;

// `coreaudio-sys` links CoreAudio but not CoreFoundation, whose string
// functions the device-name lookup needs. `cpal` used to pull it in
// indirectly; say so directly now that this backend stands alone.
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {}

pub(crate) fn scope(direction: Direction) -> u32 {
    match direction {
        Direction::Input => kAudioObjectPropertyScopeInput,
        Direction::Output => kAudioObjectPropertyScopeOutput,
    }
}

pub(crate) fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMaster,
    }
}

pub(crate) fn all_devices() -> Vec<AudioObjectID> {
    property_array(
        kAudioObjectSystemObject,
        kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
    )
}

/// A device's streams in one direction; empty if it has none.
pub(crate) fn streams(device: AudioObjectID, direction: Direction) -> Vec<AudioObjectID> {
    property_array(device, kAudioDevicePropertyStreams, scope(direction))
}

pub(crate) fn default_device(direction: Direction) -> Option<AudioObjectID> {
    let selector = match direction {
        Direction::Input => kAudioHardwarePropertyDefaultInputDevice,
        Direction::Output => kAudioHardwarePropertyDefaultOutputDevice,
    };
    property_value::<AudioObjectID>(
        kAudioObjectSystemObject,
        selector,
        kAudioObjectPropertyScopeGlobal,
    )
    .filter(|device| *device != 0)
}

pub(crate) fn device_name(device: AudioObjectID) -> Option<String> {
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
    // it exactly once.
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

// Real nominal rates are whole numbers far below u32::MAX.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn rate_hz(rate: f64) -> u32 {
    rate.round() as u32
}

pub(crate) fn nominal_sample_rate(device: AudioObjectID) -> Option<u32> {
    property_value::<f64>(
        device,
        kAudioDevicePropertyNominalSampleRate,
        kAudioObjectPropertyScopeGlobal,
    )
    .map(rate_hz)
}

/// The formats a direction's streams deliver to this process, in stream order.
pub(crate) fn virtual_formats(
    device: AudioObjectID,
    direction: Direction,
) -> Vec<AudioStreamBasicDescription> {
    streams(device, direction)
        .into_iter()
        .filter_map(|stream| {
            property_value::<AudioStreamBasicDescription>(
                stream,
                kAudioStreamPropertyVirtualFormat,
                kAudioObjectPropertyScopeGlobal,
            )
        })
        .collect()
}

pub(crate) fn buffer_frame_size(device: AudioObjectID) -> Option<u32> {
    property_value::<u32>(
        device,
        kAudioDevicePropertyBufferFrameSize,
        kAudioObjectPropertyScopeGlobal,
    )
}

/// The largest IO buffer the device may ever hand a callback.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn max_buffer_frame_size(device: AudioObjectID) -> Option<u32> {
    property_value::<AudioValueRange>(
        device,
        kAudioDevicePropertyBufferFrameSizeRange,
        kAudioObjectPropertyScopeGlobal,
    )
    .map(|range| range.mMaximum.round() as u32)
}

/// Asks for `frames` per IO callback, clamped to what the device allows.
/// CoreAudio scopes this setting to the calling process.
pub(crate) fn set_buffer_frame_size(device: AudioObjectID, frames: u32) -> Result<(), OSStatus> {
    let clamped = property_value::<AudioValueRange>(
        device,
        kAudioDevicePropertyBufferFrameSizeRange,
        kAudioObjectPropertyScopeGlobal,
    )
    .map_or(frames, |range| {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let clamp = |value: f64| value.round() as u32;
        frames.clamp(clamp(range.mMinimum), clamp(range.mMaximum).max(1))
    });
    set_property_value(
        device,
        kAudioDevicePropertyBufferFrameSize,
        kAudioObjectPropertyScopeGlobal,
        &clamped,
    )
}

pub(crate) fn is_ok(status: OSStatus) -> bool {
    status == kAudioHardwareNoError as OSStatus
}

/// Reads a fixed-size property. `T` must be the plain-data type CoreAudio
/// documents for `selector`.
pub(crate) fn property_value<T: Copy + Default>(
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
    is_ok(status).then_some(value)
}

/// Writes a fixed-size property. `T` must be the plain-data type CoreAudio
/// documents for `selector`.
pub(crate) fn set_property_value<T: Copy>(
    object: AudioObjectID,
    selector: u32,
    scope: u32,
    value: &T,
) -> Result<(), OSStatus> {
    let address = address(selector, scope);
    let size = u32::try_from(mem::size_of::<T>()).unwrap_or(u32::MAX);
    // SAFETY: `value` is a fully initialized `T` of exactly `size` bytes.
    let status = unsafe {
        AudioObjectSetPropertyData(
            object,
            &address,
            0,
            ptr::null(),
            size,
            ptr::from_ref(value).cast::<c_void>(),
        )
    };
    if is_ok(status) {
        Ok(())
    } else {
        Err(status)
    }
}

/// Reads a variable-length array property. `T` must be the plain-data
/// element type CoreAudio documents for `selector`.
pub(crate) fn property_array<T: Copy>(object: AudioObjectID, selector: u32, scope: u32) -> Vec<T> {
    let address = address(selector, scope);
    let mut size = 0u32;
    // SAFETY: querying a size writes only into `size`.
    let status =
        unsafe { AudioObjectGetPropertyDataSize(object, &address, 0, ptr::null(), &mut size) };
    let element_size = mem::size_of::<T>();
    if !is_ok(status) || element_size == 0 {
        return Vec::new();
    }
    let capacity = size as usize / element_size;
    let mut values: Vec<T> = Vec::with_capacity(capacity);
    let mut filled = u32::try_from(capacity * element_size).unwrap_or(0);
    // SAFETY: `values` has room for `capacity` elements (`filled` bytes, a
    // whole multiple of `T`'s size, so alignment holds); CoreAudio writes at
    // most `filled` bytes and reports how many it wrote. Only fully written
    // elements are exposed via `set_len`.
    unsafe {
        let status = AudioObjectGetPropertyData(
            object,
            &address,
            0,
            ptr::null(),
            &mut filled,
            values.as_mut_ptr().cast::<c_void>(),
        );
        if !is_ok(status) {
            return Vec::new();
        }
        values.set_len((filled as usize / element_size).min(capacity));
    }
    values
}
