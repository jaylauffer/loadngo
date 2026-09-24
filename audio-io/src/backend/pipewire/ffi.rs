//! Public PipeWire 0.3 ABI subset, checked against the 1.4.2 headers.
//! Loaded dynamically so missing desktop services give errors, not loader
//! failures. No private APIs, shell commands or ALSA bridge in this path.
use libloading::Library;
use std::ffi::{c_char, c_int, c_void};

#[repr(C)]
pub struct Pod {
    pub size: u32,
    pub kind: u32,
}
#[repr(C)]
pub struct Chunk {
    pub offset: u32,
    pub size: u32,
    pub stride: i32,
    pub flags: i32,
}
#[repr(C)]
pub struct Data {
    pub kind: u32,
    pub flags: u32,
    pub fd: i64,
    pub mapoffset: u32,
    pub maxsize: u32,
    pub data: *mut c_void,
    pub chunk: *mut Chunk,
}
#[repr(C)]
pub struct Buffer {
    pub n_metas: u32,
    pub n_datas: u32,
    pub metas: *mut c_void,
    pub datas: *mut Data,
}
// Only the prefix through requested is accessed (PipeWire >= 0.3.49).
#[repr(C)]
pub struct PwBuffer {
    pub buffer: *mut Buffer,
    pub user_data: *mut c_void,
    pub size: u64,
    pub requested: u64,
}
#[repr(C)]
pub struct Events {
    pub version: u32,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub state_changed: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int, *const c_char)>,
    pub control_info: Option<unsafe extern "C" fn(*mut c_void, u32, *const c_void)>,
    pub io_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *mut c_void, u32)>,
    pub param_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *const Pod)>,
    pub add_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut PwBuffer)>,
    pub remove_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut PwBuffer)>,
    pub process: Option<unsafe extern "C" fn(*mut c_void)>,
    pub drained: Option<unsafe extern "C" fn(*mut c_void)>,
    pub command: Option<unsafe extern "C" fn(*mut c_void, *const c_void)>,
    pub trigger_done: Option<unsafe extern "C" fn(*mut c_void)>,
}

macro_rules! api {
    ($($name:ident: $ty:ty),+ $(,)?) => {
        pub struct Api { $(pub $name: $ty,)+ _library: Library }
        impl Api {
            pub fn load() -> Result<Self, String> {
                // SAFETY: system ABI library; function pointer types below
                // match the public headers. The library outlives every call.
                unsafe {
                    let library = Library::new("libpipewire-0.3.so.0").map_err(|e| e.to_string())?;
                    Ok(Self { $($name: *library.get::<$ty>(concat!(stringify!($name), "\0").as_bytes()).map_err(|e| e.to_string())?,)+ _library: library })
                }
            }
        }
    };
}
api! {
    pw_init: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char),
    pw_get_library_version: unsafe extern "C" fn() -> *const c_char,
    pw_thread_loop_new: unsafe extern "C" fn(*const c_char, *const c_void) -> *mut c_void,
    pw_thread_loop_get_loop: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    pw_thread_loop_start: unsafe extern "C" fn(*mut c_void) -> c_int,
    pw_thread_loop_stop: unsafe extern "C" fn(*mut c_void),
    pw_thread_loop_destroy: unsafe extern "C" fn(*mut c_void),
    pw_properties_new_string: unsafe extern "C" fn(*const c_char) -> *mut c_void,
    pw_properties_set: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int,
    pw_properties_free: unsafe extern "C" fn(*mut c_void),
    pw_stream_new_simple: unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void, *const Events, *mut c_void) -> *mut c_void,
    pw_stream_connect: unsafe extern "C" fn(*mut c_void, c_int, u32, u32, *const *const Pod, u32) -> c_int,
    pw_stream_destroy: unsafe extern "C" fn(*mut c_void),
    pw_stream_dequeue_buffer: unsafe extern "C" fn(*mut c_void) -> *mut PwBuffer,
    pw_stream_queue_buffer: unsafe extern "C" fn(*mut c_void, *mut PwBuffer) -> c_int,
}
