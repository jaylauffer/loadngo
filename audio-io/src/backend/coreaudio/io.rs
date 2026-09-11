//! Device I/O on raw HAL IOProcs.
//!
//! `AudioDeviceCreateIOProcID` registers a C callback the HAL invokes on its
//! real-time IO thread with the device's buffers in their virtual (32-bit
//! float) format. There is no AudioUnit and no conversion stage: what the
//! callback sees is what the device delivers.
//!
//! Ownership and threads:
//! - `IoProcState` (the user callback plus scratch space) is touched only by
//!   the IO thread, from start until `AudioDeviceStop` returns.
//! - `IoStatus` (atomics) is shared with the property-listener thread and the
//!   handle, and never aliased mutably.
//! - Dropping [`Stream`] stops the IOProc (synchronous when called off the IO
//!   thread), destroys it, removes the listeners, and only then frees state.
//!
//! The IO proc never allocates: scratch space for interleaving multi-stream
//! devices is sized from the device's maximum buffer size up front, and a
//! single-stream device is handed to the callback in place with no copy.
//! Panics are caught at the FFI boundary; the stream then outputs silence and
//! reports itself failed.

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::{self, NonNull};
use std::slice;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use coreaudio_sys::{
    kAudioDeviceProcessorOverload, kAudioDevicePropertyDeviceIsAlive,
    kAudioObjectPropertyScopeGlobal, AudioBuffer, AudioBufferList, AudioDeviceCreateIOProcID,
    AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioObjectAddPropertyListener, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectRemovePropertyListener, AudioTimeStamp, OSStatus,
};

use crate::backend::{Direction, InputCallback, OutputCallback, StreamFormat};
use crate::error::AudioIoError;

use super::{devices, hal};

/// Upper bound on scratch space, whatever a device claims its maximum
/// buffer is.
const MAX_SCRATCH_FRAMES: usize = 16_384;
const LISTENED_PROPERTIES: [u32; 2] = [
    kAudioDevicePropertyDeviceIsAlive,
    kAudioDeviceProcessorOverload,
];

#[derive(Debug, Default)]
struct IoStatus {
    dead: AtomicBool,
    panicked: AtomicBool,
    overloads: AtomicU64,
    callbacks: AtomicU64,
}

enum Callback {
    Input(InputCallback),
    Output(OutputCallback),
}

struct IoProcState {
    callback: Callback,
    scratch: Vec<f32>,
    status: Arc<IoStatus>,
}

/// A running IOProc on one device. Stops when dropped.
pub(crate) struct Stream {
    device: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    state: NonNull<IoProcState>,
    status: Arc<IoStatus>,
    listening: bool,
}

// SAFETY: the HAL's device/IOProc handles aren't tied to the thread that
// created them, and `state` is only dereferenced by the IO thread while the
// proc runs; the handle merely frees it after `AudioDeviceStop` returns.
unsafe impl Send for Stream {}

impl Stream {
    /// `Some(reason)` once the device has gone away or the callback panicked.
    pub(crate) fn failure(&self) -> Option<String> {
        if self.status.dead.load(Ordering::Relaxed) {
            Some("the audio device was disconnected".to_string())
        } else if self.status.panicked.load(Ordering::Relaxed) {
            Some("the audio callback panicked and was silenced".to_string())
        } else {
            None
        }
    }

    /// How many times the HAL reported the IO thread missing its deadline.
    pub(crate) fn overloads(&self) -> u64 {
        self.status.overloads.load(Ordering::Relaxed)
    }
}

pub(crate) fn open_input(
    name: Option<&str>,
    preferred_buffer_frames: Option<u32>,
    callback: InputCallback,
) -> Result<(Stream, StreamFormat), AudioIoError> {
    open(
        Direction::Input,
        name,
        preferred_buffer_frames,
        Callback::Input(callback),
    )
}

pub(crate) fn open_output(
    name: Option<&str>,
    preferred_buffer_frames: Option<u32>,
    callback: OutputCallback,
) -> Result<(Stream, StreamFormat), AudioIoError> {
    open(
        Direction::Output,
        name,
        preferred_buffer_frames,
        Callback::Output(callback),
    )
}

fn open(
    direction: Direction,
    name: Option<&str>,
    preferred_buffer_frames: Option<u32>,
    callback: Callback,
) -> Result<(Stream, StreamFormat), AudioIoError> {
    let device = devices::resolve(direction, name)?;
    if let Some(frames) = preferred_buffer_frames {
        // Best effort: a device that refuses keeps its own size, which the
        // returned format reports.
        let _ = hal::set_buffer_frame_size(device, frames);
    }
    let format = devices::stream_format(device, direction)?;

    let max_frames = hal::max_buffer_frame_size(device)
        .map_or(MAX_SCRATCH_FRAMES, |frames| frames as usize)
        .min(MAX_SCRATCH_FRAMES);
    let status = Arc::new(IoStatus::default());
    let state = Box::new(IoProcState {
        callback,
        scratch: Vec::with_capacity(max_frames * usize::from(format.channels.max(1))),
        status: status.clone(),
    });
    let state = NonNull::from(Box::leak(state));

    let mut proc_id: AudioDeviceIOProcID = None;
    // SAFETY: `io_proc` matches `AudioDeviceIOProc`, and `state` stays valid
    // until `Stream::drop` has stopped and destroyed the proc.
    let status_code = unsafe {
        AudioDeviceCreateIOProcID(
            device,
            Some(io_proc),
            state.as_ptr().cast::<c_void>(),
            &mut proc_id,
        )
    };
    if !hal::is_ok(status_code) || proc_id.is_none() {
        // SAFETY: the proc was never registered, so nothing else holds `state`.
        drop(unsafe { Box::from_raw(state.as_ptr()) });
        return Err(stream_error("create an IO proc on", &format, status_code));
    }

    let mut stream = Stream {
        device,
        proc_id,
        state,
        status,
        listening: false,
    };
    stream.listen();
    // SAFETY: `proc_id` was just created on `device`.
    let started = unsafe { AudioDeviceStart(device, proc_id) };
    if !hal::is_ok(started) {
        return Err(stream_error("start", &format, started));
    }
    Ok((stream, format))
}

fn stream_error(action: &str, format: &StreamFormat, status: OSStatus) -> AudioIoError {
    AudioIoError::Stream(format!(
        "couldn't {action} {} (OSStatus {status})",
        format.device_name
    ))
}

impl Stream {
    fn listen(&mut self) {
        let client = Arc::as_ptr(&self.status).cast_mut().cast::<c_void>();
        for selector in LISTENED_PROPERTIES {
            let address = hal::address(selector, kAudioObjectPropertyScopeGlobal);
            // SAFETY: `listener` matches `AudioObjectPropertyListenerProc`;
            // `client` points at `IoStatus`, which `self.status` keeps alive
            // until the listeners are removed in `drop`.
            unsafe {
                AudioObjectAddPropertyListener(self.device, &address, Some(listener), client);
            }
        }
        self.listening = true;
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: stop is synchronous off the IO thread, so once it returns
        // the proc is not running and nothing else touches `state`.
        unsafe {
            AudioDeviceStop(self.device, self.proc_id);
            AudioDeviceDestroyIOProcID(self.device, self.proc_id);
        }
        if self.listening {
            let client = Arc::as_ptr(&self.status).cast_mut().cast::<c_void>();
            for selector in LISTENED_PROPERTIES {
                let address = hal::address(selector, kAudioObjectPropertyScopeGlobal);
                // SAFETY: removes exactly the listeners `listen` added.
                unsafe {
                    AudioObjectRemovePropertyListener(
                        self.device,
                        &address,
                        Some(listener),
                        client,
                    );
                }
            }
        }
        // SAFETY: the proc is destroyed; this is the only owner of `state`.
        drop(unsafe { Box::from_raw(self.state.as_ptr()) });
    }
}

/// The buffers in an `AudioBufferList`, as a slice.
///
/// # Safety
/// `list` must point at a valid list whose `mNumberBuffers` buffers are laid
/// out contiguously from `mBuffers`, as the HAL guarantees.
unsafe fn buffers<'a>(list: *const AudioBufferList) -> &'a [AudioBuffer] {
    let count = (*list).mNumberBuffers as usize;
    slice::from_raw_parts(ptr::addr_of!((*list).mBuffers).cast::<AudioBuffer>(), count)
}

fn frames_in(buffer: &AudioBuffer) -> usize {
    let channels = buffer.mNumberChannels.max(1) as usize;
    buffer.mDataByteSize as usize / size_of::<f32>() / channels
}

unsafe extern "C" fn io_proc(
    _device: AudioObjectID,
    _now: *const AudioTimeStamp,
    input: *const AudioBufferList,
    _input_time: *const AudioTimeStamp,
    output: *mut AudioBufferList,
    _output_time: *const AudioTimeStamp,
    client: *mut c_void,
) -> OSStatus {
    // SAFETY: `client` is the `IoProcState` registered with this proc, and
    // only this IO thread uses it while the proc runs.
    let state = unsafe { &mut *client.cast::<IoProcState>() };
    state.status.callbacks.fetch_add(1, Ordering::Relaxed);
    let outcome = catch_unwind(AssertUnwindSafe(|| match &mut state.callback {
        Callback::Input(callback) => {
            if !input.is_null() {
                // SAFETY: the HAL hands a valid input list for this call.
                unsafe { deliver_input(callback, &mut state.scratch, input) };
            }
        }
        Callback::Output(callback) => {
            if !output.is_null() {
                // SAFETY: the HAL hands a valid, writable output list.
                unsafe { fill_output(callback, &mut state.scratch, output) };
            }
        }
    }));
    if outcome.is_err() {
        state.status.panicked.store(true, Ordering::Relaxed);
        if !output.is_null() {
            // SAFETY: as above; silence whatever was half-written.
            unsafe { silence(output) };
        }
    }
    0
}

/// # Safety
/// `list` must be a valid input buffer list of 32-bit float samples.
unsafe fn deliver_input(
    callback: &mut InputCallback,
    scratch: &mut Vec<f32>,
    list: *const AudioBufferList,
) {
    let buffers = buffers(list);
    match buffers {
        [] => {}
        [only] if !only.mData.is_null() => {
            let samples = slice::from_raw_parts(
                only.mData.cast::<f32>(),
                only.mDataByteSize as usize / size_of::<f32>(),
            );
            callback(samples, only.mNumberChannels.max(1) as usize);
        }
        many => {
            let channels: usize = many.iter().map(|b| b.mNumberChannels.max(1) as usize).sum();
            let frames = many
                .iter()
                .map(frames_in)
                .min()
                .unwrap_or(0)
                .min(scratch.capacity() / channels.max(1));
            scratch.clear();
            for frame in 0..frames {
                for buffer in many {
                    if buffer.mData.is_null() {
                        continue;
                    }
                    let width = buffer.mNumberChannels.max(1) as usize;
                    let data = buffer.mData.cast::<f32>();
                    for channel in 0..width {
                        scratch.push(*data.add(frame * width + channel));
                    }
                }
            }
            callback(scratch, channels);
        }
    }
}

/// # Safety
/// `list` must be a valid, writable output buffer list of 32-bit float
/// samples.
unsafe fn fill_output(
    callback: &mut OutputCallback,
    scratch: &mut Vec<f32>,
    list: *mut AudioBufferList,
) {
    let buffers = buffers(list);
    match buffers {
        [] => {}
        [only] if !only.mData.is_null() => {
            let samples = slice::from_raw_parts_mut(
                only.mData.cast::<f32>(),
                only.mDataByteSize as usize / size_of::<f32>(),
            );
            callback(samples, only.mNumberChannels.max(1) as usize);
        }
        many => {
            let channels: usize = many.iter().map(|b| b.mNumberChannels.max(1) as usize).sum();
            let frames = many
                .iter()
                .map(frames_in)
                .min()
                .unwrap_or(0)
                .min(scratch.capacity() / channels.max(1));
            scratch.clear();
            scratch.resize(frames * channels, 0.0);
            callback(scratch, channels);
            let mut offset = 0;
            for buffer in many {
                let width = buffer.mNumberChannels.max(1) as usize;
                if !buffer.mData.is_null() {
                    let data = buffer.mData.cast::<f32>();
                    for frame in 0..frames {
                        for channel in 0..width {
                            *data.add(frame * width + channel) =
                                scratch[frame * channels + offset + channel];
                        }
                    }
                }
                offset += width;
            }
        }
    }
}

/// # Safety
/// `list` must be a valid, writable output buffer list.
unsafe fn silence(list: *mut AudioBufferList) {
    for buffer in buffers(list) {
        if !buffer.mData.is_null() {
            ptr::write_bytes(buffer.mData.cast::<u8>(), 0, buffer.mDataByteSize as usize);
        }
    }
}

unsafe extern "C" fn listener(
    device: AudioObjectID,
    count: u32,
    addresses: *const AudioObjectPropertyAddress,
    client: *mut c_void,
) -> OSStatus {
    // SAFETY: `client` is the `IoStatus` the stream registered, kept alive
    // until the listener is removed; only its atomics are touched.
    let status = unsafe { &*client.cast::<IoStatus>() };
    // SAFETY: the HAL passes `count` valid addresses.
    let addresses = unsafe { slice::from_raw_parts(addresses, count as usize) };
    for address in addresses {
        if address.mSelector == kAudioDevicePropertyDeviceIsAlive {
            let alive = hal::property_value::<u32>(
                device,
                kAudioDevicePropertyDeviceIsAlive,
                kAudioObjectPropertyScopeGlobal,
            );
            if alive != Some(1) {
                status.dead.store(true, Ordering::Relaxed);
            }
        } else if address.mSelector == kAudioDeviceProcessorOverload {
            status.overloads.fetch_add(1, Ordering::Relaxed);
        }
    }
    0
}
