//! Session-managed Linux playback using PipeWire's public C ABI from Rust.
//! All callbacks are serialized on one PipeWire-owned event-loop thread
//! (RT_PROCESS is deliberately not set). No polling or per-frame threads.
mod ffi;
mod pod;

use std::cell::{Cell, UnsafeCell};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::marker::PhantomData;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::Duration;

use crate::backend::OutputCallback;
use crate::{AudioIoError, DesktopOutputOptions, OutputFormat};

static API: OnceLock<Result<ffi::Api, String>> = OnceLock::new();
const MAX_FRAMES: usize = 8192;

fn api() -> Result<&'static ffi::Api, AudioIoError> {
    API.get_or_init(|| {
        let api = ffi::Api::load()?;
        // SAFETY: function loaded from the public ABI returns a static string.
        let version = unsafe { CStr::from_ptr((api.pw_get_library_version)()) }.to_string_lossy();
        if !supported_version(&version) {
            return Err(format!("PipeWire {version} is too old; need >= 0.3.49"));
        }
        // SAFETY: null arguments mean no argv parsing. Called once and kept
        // initialized for process lifetime; never deinitializes other users.
        unsafe { (api.pw_init)(ptr::null_mut(), ptr::null_mut()) };
        Ok(api)
    })
    .as_ref()
    .map_err(|e| AudioIoError::Backend(format!("native PipeWire unavailable: {e}")))
}

fn supported_version(version: &str) -> bool {
    let mut parts = version.split('.').map(str::parse::<u32>);
    matches!((parts.next(), parts.next(), parts.next()),
        (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch))) if (major, minor, patch) >= (0, 3, 49))
}

struct Context {
    api: &'static ffi::Api,
    stream: *mut c_void,
    fill: OutputCallback,
    failure: Arc<AtomicU8>,
    ready: Option<mpsc::SyncSender<Result<(), &'static str>>>,
    format_valid: bool,
    streaming: bool,
}

impl Context {
    fn fail(&mut self, code: u8) {
        let _ = self
            .failure
            .compare_exchange(0, code, Ordering::Relaxed, Ordering::Relaxed);
        if let Some(ready) = self.ready.take() {
            let _ = ready.try_send(Err(failure_message(code)));
        }
    }

    fn signal_ready(&mut self) {
        if self.streaming && self.format_valid {
            if let Some(ready) = self.ready.take() {
                let _ = ready.try_send(Ok(()));
            }
        }
    }
}

fn failure_message(code: u8) -> &'static str {
    match code {
        1 => "PipeWire stream disconnected or server rejected the connection",
        2 => "desktop audio callback panicked; output silenced",
        3 => "PipeWire negotiated an unsupported audio format",
        4 => "PipeWire supplied an invalid or oversized audio buffer",
        5 => "PipeWire could not queue an audio buffer",
        _ => "unknown PipeWire stream error",
    }
}

pub(crate) struct Stream {
    api: &'static ffi::Api,
    loop_: *mut c_void,
    stream: *mut c_void,
    running: bool,
    // UnsafeCell explicitly permits foreign callbacks to mutate state. It
    // stays at a stable address and is released only after loop stop/join.
    _context: Box<UnsafeCell<Context>>,
    failure: Arc<AtomicU8>,
    _not_sync: PhantomData<Cell<()>>,
}

// SAFETY: no API operations race: creation precedes loop start, callbacks
// are serialized by PipeWire, drop stops/joins before destruction, and the
// only externally read state is atomic. The callback itself is Send.
unsafe impl Send for Stream {}

impl Stream {
    pub(crate) fn failure(&self) -> Option<String> {
        let code = self.failure.load(Ordering::Relaxed);
        (code != 0).then(|| failure_message(code).to_string())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: stop joins the event thread without holding its lock. All
        // callbacks have finished before stream/context/library destruction.
        unsafe {
            if self.running {
                (self.api.pw_thread_loop_stop)(self.loop_);
            }
            if !self.stream.is_null() {
                (self.api.pw_stream_destroy)(self.stream);
            }
            if !self.loop_.is_null() {
                (self.api.pw_thread_loop_destroy)(self.loop_);
            }
        }
    }
}

pub(crate) fn open(
    options: &DesktopOutputOptions,
    fill: OutputCallback,
) -> Result<(Stream, OutputFormat), AudioIoError> {
    let api = api()?;
    let name = CString::new(options.application_name.as_str())
        .map_err(|e| AudioIoError::Stream(e.to_string()))?;
    let (tx, rx) = mpsc::sync_channel(1);
    let failure = Arc::new(AtomicU8::new(0));
    let context = Box::new(UnsafeCell::new(Context {
        api,
        stream: ptr::null_mut(),
        fill,
        failure: failure.clone(),
        ready: Some(tx),
        format_valid: false,
        streaming: false,
    }));
    let mut owned = Stream {
        api,
        loop_: ptr::null_mut(),
        stream: ptr::null_mut(),
        running: false,
        _context: context,
        failure,
        _not_sync: PhantomData,
    };
    // SAFETY: all configuration happens before the loop thread starts. C
    // strings/PODs remain alive through calls which copy their contents.
    unsafe {
        owned.loop_ = (api.pw_thread_loop_new)(c"loadngo-desktop-audio".as_ptr(), ptr::null());
        if owned.loop_.is_null() {
            return Err(AudioIoError::Stream(
                "could not create PipeWire loop".into(),
            ));
        }
        let props = (api.pw_properties_new_string)(
            c"media.type=Audio media.category=Playback media.role=Game node.always-process=false"
                .as_ptr(),
        );
        if props.is_null() {
            return Err(AudioIoError::Stream(
                "could not create PipeWire properties".into(),
            ));
        }
        let configure = || -> Result<(), AudioIoError> {
            for (key, value) in [
                (c"application.name", Some(options.application_name.clone())),
                (
                    c"media.name",
                    Some(format!("{} audio", options.application_name)),
                ),
                (c"target.object", options.target.clone()),
                (
                    c"node.latency",
                    options
                        .preferred_buffer_frames
                        .map(|n| format!("{n}/{}", pod::RATE)),
                ),
            ] {
                if let Some(value) = value {
                    let value =
                        CString::new(value).map_err(|e| AudioIoError::Stream(e.to_string()))?;
                    if (api.pw_properties_set)(props, key.as_ptr(), value.as_ptr()) < 0 {
                        return Err(AudioIoError::Stream(
                            "could not set PipeWire properties".into(),
                        ));
                    }
                }
            }
            Ok(())
        };
        if let Err(error) = configure() {
            (api.pw_properties_free)(props);
            return Err(error);
        }
        // new_simple takes ownership of props, including on failure.
        owned.stream = (api.pw_stream_new_simple)(
            (api.pw_thread_loop_get_loop)(owned.loop_),
            name.as_ptr(),
            props,
            &EVENTS,
            owned._context.get().cast(),
        );
        if owned.stream.is_null() {
            return Err(AudioIoError::Stream(
                "could not create PipeWire stream".into(),
            ));
        }
        (*owned._context.get()).stream = owned.stream;
        let format = pod::format();
        let param = (&format as *const pod::FormatPod).cast::<ffi::Pod>();
        // OUTPUT=1, AUTOCONNECT=1, MAP_BUFFERS=4. No exclusive-device flag;
        // no RT_PROCESS: control and process callbacks share one event loop.
        let result = (api.pw_stream_connect)(owned.stream, 1, u32::MAX, 1 | 4, &param, 1);
        if result < 0 {
            return Err(AudioIoError::Stream(format!(
                "PipeWire connect failed ({result}); desktop session/server required"
            )));
        }
        let result = (api.pw_thread_loop_start)(owned.loop_);
        if result < 0 {
            return Err(AudioIoError::Stream(format!(
                "PipeWire loop start failed ({result})"
            )));
        }
        owned.running = true;
    }
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(AudioIoError::Stream(error.into())),
        Err(_) => {
            return Err(AudioIoError::Stream(
                "PipeWire output not ready within 3 seconds; check the desktop output/target"
                    .into(),
            ))
        }
    }
    let format = OutputFormat {
        device_name: options
            .target
            .clone()
            .unwrap_or_else(|| "PipeWire session default".into()),
        sample_rate_hz: pod::RATE,
        channels: 2,
        buffer_frames: None,
    };
    Ok((owned, format))
}

unsafe extern "C" fn state_changed(
    data: *mut c_void,
    _old: c_int,
    state: c_int,
    _error: *const c_char,
) {
    // SAFETY: stable context from new_simple, serialized on the loop thread.
    let context = unsafe { &mut *data.cast::<Context>() };
    if state == -1 || (state == 0 && context.ready.is_none()) {
        context.fail(1);
    }
    context.streaming = state == 3;
    context.signal_ready();
}

unsafe extern "C" fn param_changed(data: *mut c_void, id: u32, param: *const ffi::Pod) {
    if id != 4 {
        return;
    } // SPA_PARAM_Format
      // SAFETY: serialized callback; PipeWire guarantees a readable POD of
      // header + size bytes. Bound parsing to 4 KiB; no unbounded traversal.
    let context = unsafe { &mut *data.cast::<Context>() };
    context.format_valid = false;
    if param.is_null() {
        return;
    }
    let size = unsafe { (*param).size } as usize;
    if size > 4096 {
        context.fail(3);
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(param.cast::<u8>(), size + 8) };
    if !pod::is_expected_format(bytes) {
        context.fail(3);
        return;
    }
    context.format_valid = true;
    context.signal_ready();
}

fn frame_count(maxsize: u32, requested: u64) -> Option<usize> {
    let capacity = maxsize as usize / 8; // two f32 channels
    let frames = if requested == 0 {
        capacity
    } else {
        capacity.min(usize::try_from(requested).ok()?)
    };
    (frames > 0 && frames <= MAX_FRAMES).then_some(frames)
}

unsafe extern "C" fn process(data: *mut c_void) {
    // SAFETY: serialized loop callback; dequeued buffers belong exclusively
    // to this stream until queued. Only the declared one-plane f32 layout
    // is accepted; sizes, writability, pointers and alignment are checked.
    let context = unsafe { &mut *data.cast::<Context>() };
    let buffer = unsafe { (context.api.pw_stream_dequeue_buffer)(context.stream) };
    if buffer.is_null() {
        return;
    }
    // No Rust user callback may unwind across the C boundary.
    unsafe {
        let spa = (*buffer).buffer;
        if !spa.is_null() && (*spa).n_datas == 1 && !(*spa).datas.is_null() {
            let plane = &mut *(*spa).datas;
            if !plane.chunk.is_null() {
                let chunk = &mut *plane.chunk;
                chunk.offset = 0;
                chunk.size = 0;
                chunk.stride = 8;
                chunk.flags = 0;
                if let Some(frames) = frame_count(plane.maxsize, (*buffer).requested) {
                    if !plane.data.is_null()
                        && is_f32_aligned(plane.data as usize)
                        && plane.flags & 2 != 0
                    {
                        let samples =
                            std::slice::from_raw_parts_mut(plane.data.cast::<f32>(), frames * 2);
                        samples.fill(0.0);
                        if context.format_valid && context.failure.load(Ordering::Relaxed) == 0 {
                            if let Err(payload) =
                                catch_unwind(AssertUnwindSafe(|| (context.fill)(samples, 2)))
                            {
                                // A panic payload may itself panic on drop.
                                std::mem::forget(payload);
                                samples.fill(0.0);
                                context.fail(2);
                            }
                        }
                        // Never send NaNs/infinities or out-of-range samples
                        // to the desktop server, even from an errant mixer.
                        for sample in samples {
                            *sample = if sample.is_finite() {
                                sample.clamp(-1.0, 1.0)
                            } else {
                                0.0
                            };
                        }
                        chunk.size = (frames * 8) as u32;
                        (*buffer).size = frames as u64;
                    } else {
                        context.fail(4);
                    }
                } else {
                    context.fail(4);
                }
            } else {
                context.fail(4);
            }
        } else {
            context.fail(4);
        }
        if (context.api.pw_stream_queue_buffer)(context.stream, buffer) < 0 {
            context.fail(5);
        }
    }
}

static EVENTS: ffi::Events = ffi::Events {
    version: 2,
    destroy: None,
    state_changed: Some(state_changed),
    control_info: None,
    io_changed: None,
    param_changed: Some(param_changed),
    add_buffer: None,
    remove_buffer: None,
    process: Some(process),
    drained: None,
    command: None,
    trigger_done: None,
};

/// Whether a buffer address can be viewed as `f32` samples.
fn is_f32_aligned(address: usize) -> bool {
    address.is_multiple_of(std::mem::align_of::<f32>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_alignment_check_rejects_misaligned_addresses() {
        assert!(is_f32_aligned(0x1000));
        assert!(is_f32_aligned(0x1004));
        assert!(!is_f32_aligned(0x1001));
        assert!(!is_f32_aligned(0x1002));
    }

    #[test]
    fn version_gate_protects_requested_buffer_field() {
        assert!(!supported_version("0.3.48"));
        assert!(!supported_version("invalid"));
        assert!(supported_version("0.3.49"));
        assert!(supported_version("1.4.2"));
    }
    #[test]
    fn buffer_math_is_bounded_and_frame_aligned() {
        assert_eq!(frame_count(4096, 128), Some(128));
        assert_eq!(frame_count(4096, 0), Some(512));
        assert_eq!(frame_count(4095, 1000), Some(511));
        assert_eq!(frame_count(7, 0), None);
        assert_eq!(frame_count(u32::MAX, u64::MAX), None);
    }
    #[test]
    fn abi_layouts_match_lp64_headers() {
        assert_eq!(std::mem::size_of::<ffi::Data>(), 40);
        assert_eq!(std::mem::size_of::<ffi::Chunk>(), 16);
        assert_eq!(std::mem::size_of::<ffi::Buffer>(), 24);
        assert_eq!(std::mem::size_of::<ffi::Events>(), 96);
        assert_eq!(std::mem::offset_of!(ffi::PwBuffer, requested), 24);
    }
}
