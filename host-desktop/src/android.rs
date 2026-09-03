use std::cell::RefCell;
use std::collections::{hash_map::DefaultHasher, HashMap, VecDeque};
use std::env;
use std::ffi::{c_char, c_void, CString};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use std::time::{Duration, Instant};

use crate::android_jni;
use jni::objects::{JObject, JValue};
use loadngo_gfx_gles::{GlesBackend, GlesBackendState};
use loadngo_host_core::{
    DecodedImage, ExclusionRect, FrameDemand, FrameTiming, HostFrame, InputSnapshot, RenderOp,
    SafeAreaInsets, SurfaceInfo, TextMetrics, TouchPhase, TouchPoint, WindowDescriptor,
    WindowIconSet,
};
use loadngo_renderer::{FrameCommand, ImageRequest, Renderer, RendererConfig};
use ndk::asset::AssetManager;
use ndk::hardware_buffer_format::HardwareBufferFormat;
use ndk::native_window::NativeWindow;
use ui_core::{
    geometry::{Color as UiColor, Rect as UiRect},
    multiline_line_step,
    paint::PaintOp,
    single_line_text_box_height,
};

#[derive(Clone, Default)]
pub struct DesktopFont {
    source_path: Option<String>,
    software_font: Option<SoftwareFont>,
}

impl DesktopFont {
    fn new(source_path: Option<String>, software_font: Option<SoftwareFont>) -> Self {
        Self {
            source_path,
            software_font,
        }
    }

    pub fn source_path(&self) -> Option<&str> {
        self.source_path.as_deref()
    }
}

#[derive(Clone)]
struct SoftwareFont {
    inner: Arc<fontdue::Font>,
}

#[derive(Clone, Default)]
pub struct DesktopTexture {
    image_key: Option<String>,
    width: f32,
    height: f32,
    software_texture: SoftwareTexture,
}

impl DesktopTexture {
    fn new(image_key: Option<String>, software_texture: SoftwareTexture) -> Self {
        Self {
            image_key,
            width: software_texture.width as f32,
            height: software_texture.height as f32,
            software_texture,
        }
    }

    pub fn width(&self) -> f32 {
        self.width
    }

    pub fn height(&self) -> f32 {
        self.height
    }

    pub fn image_key(&self) -> Option<&str> {
        self.image_key.as_deref()
    }
}

#[derive(Clone, Default)]
struct SoftwareTexture {
    width: usize,
    height: usize,
    rgba8: Arc<[u8]>,
}

impl SoftwareTexture {
    fn from_decoded_image(image: &DecodedImage) -> Self {
        Self {
            width: image.width as usize,
            height: image.height as usize,
            rgba8: Arc::from(image.rgba8.clone()),
        }
    }

    fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0 || self.rgba8.is_empty()
    }

    fn to_gles_resource(&self) -> loadngo_gfx_gles::GlesImageResource {
        loadngo_gfx_gles::GlesImageResource {
            width: self.width as i32,
            height: self.height as i32,
            rgba8: self.rgba8.clone(),
            identity: self.rgba8.as_ptr() as usize,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopRenderBackendKind {
    Gles,
    Software,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopRenderBackendStatus {
    pub requested: DesktopRenderBackendKind,
    pub last_used: DesktopRenderBackendKind,
    pub metal_initialized: bool,
    pub metal_surface_bound: bool,
    pub detail: String,
}

enum ReactorMessage {
    LaunchFactory(
        Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + 'static>> + Send + 'static>,
    ),
    WindowCreated(usize),
    WindowDestroyed,
    InputQueueCreated(usize),
    InputQueueDestroyed,
    Stop,
}

enum InputThreadMessage {
    AttachQueue(usize),
    DetachQueue,
    Stop,
}

struct AndroidAppState {
    activity_ptr: Option<usize>,
    asset_manager_ptr: Option<usize>,
    internal_data_path: Option<PathBuf>,
    display_scale: f32,
    /// Mirrors the platform's own foreground/background signal: set to
    /// `false` by `onPause` (screen lock, task switch, an incoming call
    /// covering the activity, ...) and back to `true` by `onResume`. Starts
    /// `true` since the activity is always foregrounded at process launch.
    foreground: bool,
    /// Real device-reserved screen space, refreshed on `onResume` and
    /// `onWindowFocusChanged(true)` (see `query_safe_area_insets`) and read
    /// cheaply here every frame, the same caching shape as `surface`.
    insets: SafeAreaInsets,
    /// Whether the game has asked for sticky-immersive presentation via
    /// `set_immersive_mode(true)`. Remembered so the flags can be silently
    /// reapplied whenever Android clears them (window focus regained,
    /// resume from lock) without the caller having to notice and re-request.
    immersive_requested: bool,
    surface: SurfaceInfo,
    input: InputSnapshot,
    /// Touch ids whose `ACTION_UP`/`ACTION_POINTER_UP` arrived before
    /// `capture_frame` ever observed their `TouchPhase::Started` — a fast
    /// tap can have both its down and up events processed by the input
    /// thread within a single frame interval. Rather than overwrite
    /// `Started` straight to `Ended` (which `capture_frame`'s decay loop
    /// would then release without any caller ever having seen `Started`,
    /// silently dropping the tap — this is exactly what "opening a chest
    /// needs multiple presses" looks like from the game side), the phase
    /// is left as `Started` for one more frame and the id is recorded
    /// here; `capture_frame` releases it immediately after that Started
    /// observation decays to `Stationary`, so every caller still sees
    /// exactly one clean `Started` frame no matter how fast the tap was.
    pending_touch_release: Vec<u64>,
    timing: FrameTiming,
    last_tick: Instant,
    frame_counter: u64,
    event_epoch: u64,
    next_frame_wakers: Vec<Waker>,
    runtime_started: bool,
    runtime_completed: bool,
    runtime_entry: Option<fn()>,
    reactor_running: bool,
    looper_ptr: Option<usize>,
    choreographer_ptr: Option<usize>,
    input_thread_running: bool,
    input_looper_ptr: Option<usize>,
    window_ptr: Option<usize>,
    window: Option<NativeWindow>,
    input_queue_ptr: Option<usize>,
    attached_input_queue_ptr: Option<usize>,
    frame_callback_scheduled: bool,
    simulate_mouse_with_touch: bool,
    queued_commands: Vec<FrameCommand>,
    texture_registry: HashMap<String, SoftwareTexture>,
    generated_texture_cache: HashMap<String, SoftwareTexture>,
    generated_texture_cache_font_id: Option<usize>,
    current_font: Option<SoftwareFont>,
    default_font: Option<SoftwareFont>,
    presented_frames: u64,
    gles_backend: Option<GlesBackend>,
    last_backend_used: DesktopRenderBackendKind,
    backend_detail: String,
    control_messages: VecDeque<ReactorMessage>,
    input_thread_messages: VecDeque<InputThreadMessage>,
}

impl Default for AndroidAppState {
    fn default() -> Self {
        Self {
            activity_ptr: None,
            asset_manager_ptr: None,
            internal_data_path: None,
            display_scale: 1.0,
            foreground: true,
            insets: SafeAreaInsets::default(),
            immersive_requested: false,
            surface: SurfaceInfo {
                width: 0.0,
                height: 0.0,
            },
            input: blank_snapshot(),
            pending_touch_release: Vec::new(),
            timing: FrameTiming {
                delta_seconds: 1.0 / 60.0,
            },
            last_tick: Instant::now(),
            frame_counter: 0,
            event_epoch: 0,
            next_frame_wakers: Vec::new(),
            runtime_started: false,
            runtime_completed: false,
            runtime_entry: None,
            reactor_running: false,
            looper_ptr: None,
            choreographer_ptr: None,
            input_thread_running: false,
            input_looper_ptr: None,
            window_ptr: None,
            window: None,
            input_queue_ptr: None,
            attached_input_queue_ptr: None,
            frame_callback_scheduled: false,
            simulate_mouse_with_touch: true,
            queued_commands: Vec::new(),
            texture_registry: HashMap::new(),
            generated_texture_cache: HashMap::new(),
            generated_texture_cache_font_id: None,
            current_font: None,
            default_font: None,
            presented_frames: 0,
            gles_backend: None,
            last_backend_used: DesktopRenderBackendKind::Unavailable,
            backend_detail: "Android host waiting for the first frame".to_string(),
            control_messages: VecDeque::new(),
            input_thread_messages: VecDeque::new(),
        }
    }
}

static APP_STATE: OnceLock<Mutex<AndroidAppState>> = OnceLock::new();
static TEXT_METRICS_CACHE: OnceLock<Mutex<HashMap<u64, TextMetrics>>> = OnceLock::new();
thread_local! {
    static MAIN_THREAD_RUNTIME_FUTURE: RefCell<Option<Pin<Box<dyn Future<Output = ()> + 'static>>>> =
        const { RefCell::new(None) };
}
unsafe extern "C" {
    fn __android_log_write(prio: i32, tag: *const c_char, text: *const c_char) -> i32;
}

const ANDROID_LOG_INFO: i32 = 4;
const ANDROID_LOG_ERROR: i32 = 6;
const AINPUT_EVENT_TYPE_KEY_I32: i32 = ndk_sys::AINPUT_EVENT_TYPE_KEY as i32;
const AINPUT_EVENT_TYPE_MOTION_I32: i32 = ndk_sys::AINPUT_EVENT_TYPE_MOTION as i32;
const AKEY_EVENT_ACTION_DOWN_I32: i32 = ndk_sys::AKEY_EVENT_ACTION_DOWN as i32;
const AKEY_EVENT_ACTION_UP_I32: i32 = ndk_sys::AKEY_EVENT_ACTION_UP as i32;
const AMOTION_EVENT_ACTION_DOWN_I32: i32 = ndk_sys::AMOTION_EVENT_ACTION_DOWN as i32;
const AMOTION_EVENT_ACTION_UP_I32: i32 = ndk_sys::AMOTION_EVENT_ACTION_UP as i32;
const AMOTION_EVENT_ACTION_MOVE_I32: i32 = ndk_sys::AMOTION_EVENT_ACTION_MOVE as i32;
const AMOTION_EVENT_ACTION_CANCEL_I32: i32 = ndk_sys::AMOTION_EVENT_ACTION_CANCEL as i32;
const AMOTION_EVENT_ACTION_POINTER_DOWN_I32: i32 =
    ndk_sys::AMOTION_EVENT_ACTION_POINTER_DOWN as i32;
const AMOTION_EVENT_ACTION_POINTER_UP_I32: i32 = ndk_sys::AMOTION_EVENT_ACTION_POINTER_UP as i32;
const AMOTION_EVENT_ACTION_POINTER_INDEX_MASK_I32: i32 =
    ndk_sys::AMOTION_EVENT_ACTION_POINTER_INDEX_MASK as i32;
const AMOTION_EVENT_ACTION_POINTER_INDEX_SHIFT_I32: i32 =
    ndk_sys::AMOTION_EVENT_ACTION_POINTER_INDEX_SHIFT as i32;
const AKEYCODE_BACK_I32: i32 = ndk_sys::AKEYCODE_BACK as i32;
const AKEYCODE_ESCAPE_I32: i32 = ndk_sys::AKEYCODE_ESCAPE as i32;
const AKEYCODE_SPACE_I32: i32 = ndk_sys::AKEYCODE_SPACE as i32;
const AKEYCODE_DPAD_CENTER_I32: i32 = ndk_sys::AKEYCODE_DPAD_CENTER as i32;
const AKEYCODE_F3_I32: i32 = ndk_sys::AKEYCODE_F3 as i32;
const AKEYCODE_R_I32: i32 = ndk_sys::AKEYCODE_R as i32;
const AKEYCODE_DPAD_UP_I32: i32 = ndk_sys::AKEYCODE_DPAD_UP as i32;
const AKEYCODE_DPAD_DOWN_I32: i32 = ndk_sys::AKEYCODE_DPAD_DOWN as i32;
const INPUT_QUEUE_IDENT: i32 = 2;

fn app_state() -> &'static Mutex<AndroidAppState> {
    APP_STATE.get_or_init(|| Mutex::new(AndroidAppState::default()))
}

fn text_metrics_cache() -> &'static Mutex<HashMap<u64, TextMetrics>> {
    TEXT_METRICS_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn android_log(prio: i32, message: &str) {
    const TAG: &[u8] = b"loadngo\0";
    let mut buffer = [0u8; 512];
    let mut len = 0usize;
    for &byte in message.as_bytes() {
        if len >= buffer.len() - 1 {
            break;
        }
        buffer[len] = if byte == 0 { b' ' } else { byte };
        len += 1;
    }
    if len == 0 {
        buffer[0] = b' ';
        len = 1;
    }
    buffer[len] = 0;
    unsafe {
        let _ = __android_log_write(prio, TAG.as_ptr().cast(), buffer.as_ptr().cast());
    }
}

pub fn android_log_info(message: &str) {
    android_log(ANDROID_LOG_INFO, message);
}

pub fn android_log_error(message: &str) {
    android_log(ANDROID_LOG_ERROR, message);
}

fn request_activity_finish() {
    let activity_ptr = {
        let state = app_state().lock().expect("android app state poisoned");
        state.activity_ptr
    };
    let Some(activity_ptr) = activity_ptr else {
        return;
    };
    let activity = activity_ptr as *mut ndk_sys::ANativeActivity;
    if activity.is_null() {
        return;
    }
    unsafe {
        ndk_sys::ANativeActivity_finish(activity);
    }
    android_log_info("Android activity finish requested");
}

fn update_backend_detail(state: &mut AndroidAppState, detail: impl Into<String>) {
    let detail = detail.into();
    if state.backend_detail != detail {
        android_log_info(&detail);
        state.backend_detail = detail;
    }
}

fn ensure_main_thread_reactor_initialized() -> bool {
    let mut state = app_state().lock().expect("android app state poisoned");
    if state.reactor_running {
        return true;
    }
    let looper = unsafe {
        let existing = ndk_sys::ALooper_forThread();
        if existing.is_null() {
            ndk_sys::ALooper_prepare(ndk_sys::ALOOPER_PREPARE_ALLOW_NON_CALLBACKS as i32)
        } else {
            existing
        }
    };
    if looper.is_null() {
        android_log_error("Android reactor failed: ALooper_prepare returned null");
        return false;
    }
    state.reactor_running = true;
    state.looper_ptr = Some(looper as usize);
    if state.choreographer_ptr.is_none() {
        let choreographer = unsafe { ndk_sys::AChoreographer_getInstance() };
        if choreographer.is_null() {
            android_log_error("Android reactor failed: AChoreographer_getInstance returned null");
            state.reactor_running = false;
            state.looper_ptr = None;
            return false;
        }
        state.choreographer_ptr = Some(choreographer as usize);
    }
    if let Some(window_ptr) = state.window_ptr {
        state
            .control_messages
            .push_back(ReactorMessage::WindowCreated(window_ptr));
    }
    if let Some(queue_ptr) = state.input_queue_ptr {
        state
            .control_messages
            .push_back(ReactorMessage::InputQueueCreated(queue_ptr));
    }
    android_log_info("Android reactor looper initialized on the activity thread");
    true
}

fn request_frame_callback() {
    let choreographer_ptr = {
        let mut state = app_state().lock().expect("android app state poisoned");
        if state.frame_callback_scheduled || !state.runtime_started || state.runtime_completed {
            return;
        }
        if state.choreographer_ptr.is_none() {
            let current_looper = unsafe { ndk_sys::ALooper_forThread() };
            if current_looper.is_null() || Some(current_looper as usize) != state.looper_ptr {
                android_log_error(
                    "Android reactor failed: main-thread choreographer unavailable for frame callback",
                );
                return;
            }
            let choreographer = unsafe { ndk_sys::AChoreographer_getInstance() };
            if choreographer.is_null() {
                android_log_error(
                    "Android reactor failed: AChoreographer_getInstance returned null",
                );
                return;
            }
            state.choreographer_ptr = Some(choreographer as usize);
        }
        state.frame_callback_scheduled = true;
        match state.choreographer_ptr {
            Some(ptr) => ptr,
            None => {
                state.frame_callback_scheduled = false;
                android_log_error("Android reactor failed: choreographer pointer missing");
                return;
            }
        }
    };
    unsafe {
        ndk_sys::AChoreographer_postFrameCallback64(
            choreographer_ptr as *mut ndk_sys::AChoreographer,
            Some(on_frame_callback),
            std::ptr::null_mut(),
        );
    }
}

fn wake_next_frame_waiters(state: &mut AndroidAppState) {
    for waker in state.next_frame_wakers.drain(..) {
        waker.wake();
    }
}

fn wake_input_thread() {
    let looper_ptr = {
        let state = app_state().lock().expect("android app state poisoned");
        state.input_looper_ptr
    };
    if let Some(looper_ptr) = looper_ptr {
        unsafe {
            ndk_sys::ALooper_wake(looper_ptr as *mut ndk_sys::ALooper);
        }
    }
}

fn android_display_scale(asset_manager_ptr: Option<usize>) -> f32 {
    let Some(asset_manager_ptr) = asset_manager_ptr else {
        return 1.0;
    };
    let config = unsafe { ndk_sys::AConfiguration_new() };
    if config.is_null() {
        return 1.0;
    }
    let density = unsafe {
        ndk_sys::AConfiguration_fromAssetManager(
            config,
            asset_manager_ptr as *mut ndk_sys::AAssetManager,
        );
        ndk_sys::AConfiguration_getDensity(config)
    };
    unsafe {
        ndk_sys::AConfiguration_delete(config);
    }
    match density as u32 {
        ndk_sys::ACONFIGURATION_DENSITY_DEFAULT => 1.0,
        ndk_sys::ACONFIGURATION_DENSITY_ANY | ndk_sys::ACONFIGURATION_DENSITY_NONE => 1.0,
        _ if density <= 0 => 1.0,
        _ => (density as f32 / 160.0).max(0.5),
    }
}

fn logical_surface_info(window: &NativeWindow, display_scale: f32) -> SurfaceInfo {
    let scale = display_scale.max(0.01);
    SurfaceInfo {
        width: (window.width() as f32 / scale).max(1.0),
        height: (window.height() as f32 / scale).max(1.0),
    }
}

fn logical_point(value: f32, display_scale: f32) -> f32 {
    value / display_scale.max(0.01)
}

fn scale_rect(rect: UiRect, scale: f32) -> UiRect {
    UiRect {
        x: rect.x * scale,
        y: rect.y * scale,
        width: rect.width * scale,
        height: rect.height * scale,
    }
}

fn scale_point(point: ui_core::geometry::Point, scale: f32) -> ui_core::geometry::Point {
    ui_core::geometry::Point {
        x: point.x * scale,
        y: point.y * scale,
    }
}

fn scale_thickness(thickness: i32, scale: f32) -> i32 {
    ((thickness.max(1) as f32) * scale).round().max(1.0) as i32
}

fn scale_particles(
    particles: &[ui_core::paint::Particle],
    scale: f32,
) -> Vec<ui_core::paint::Particle> {
    particles
        .iter()
        .map(|particle| ui_core::paint::Particle {
            center: scale_point(particle.center, scale),
            radius: particle.radius * scale,
            color: particle.color,
        })
        .collect()
}

fn scale_frame_command(command: &FrameCommand, scale: f32) -> FrameCommand {
    match command {
        FrameCommand::Clear { color } => FrameCommand::Clear { color: *color },
        FrameCommand::FillRect { rect, color } => FrameCommand::FillRect {
            rect: scale_rect(*rect, scale),
            color: *color,
        },
        FrameCommand::StrokeRect {
            rect,
            color,
            thickness,
        } => FrameCommand::StrokeRect {
            rect: scale_rect(*rect, scale),
            color: *color,
            thickness: scale_thickness(*thickness, scale),
        },
        FrameCommand::Line {
            from,
            to,
            color,
            thickness,
        } => FrameCommand::Line {
            from: scale_point(*from, scale),
            to: scale_point(*to, scale),
            color: *color,
            thickness: scale_thickness(*thickness, scale),
        },
        FrameCommand::Circle {
            center,
            radius,
            color,
        } => FrameCommand::Circle {
            center: scale_point(*center, scale),
            radius: radius * scale,
            color: *color,
        },
        FrameCommand::Polyline {
            points,
            color,
            thickness,
            closed,
        } => FrameCommand::Polyline {
            points: points
                .iter()
                .map(|point| scale_point(*point, scale))
                .collect(),
            color: *color,
            thickness: scale_thickness(*thickness, scale),
            closed: *closed,
        },
        FrameCommand::Arc {
            center,
            radius,
            start_angle,
            sweep_angle,
            color,
            thickness,
        } => FrameCommand::Arc {
            center: scale_point(*center, scale),
            radius: radius * scale,
            start_angle: *start_angle,
            sweep_angle: *sweep_angle,
            color: *color,
            thickness: scale_thickness(*thickness, scale),
        },
        FrameCommand::ParticleBatch { particles } => FrameCommand::ParticleBatch {
            particles: scale_particles(particles, scale),
        },
        FrameCommand::Text(request) => FrameCommand::Text(loadngo_renderer::TextRequest {
            rect: scale_rect(request.rect, scale),
            clip_rect: request.clip_rect.map(|rect| scale_rect(rect, scale)),
            text: request.text.clone(),
            style: loadngo_host_core::RenderTextStyle {
                font_size: ((request.style.font_size.max(1) as f32) * scale)
                    .round()
                    .max(1.0) as u16,
                color: request.style.color,
                horizontal_align: request.style.horizontal_align.clone(),
                vertical_align: request.style.vertical_align.clone(),
                vertical_metric_mode: request.style.vertical_metric_mode.clone(),
                layout_mode: request.style.layout_mode.clone(),
                overflow: request.style.overflow.clone(),
            },
            font_source: request.font_source.clone(),
            direction: request.direction,
            script: request.script,
            language: request.language.clone(),
        }),
        FrameCommand::Image(request) => FrameCommand::Image(ImageRequest {
            rect: scale_rect(request.rect, scale),
            clip_rect: request.clip_rect.map(|rect| scale_rect(rect, scale)),
            image_key: request.image_key.clone(),
            alpha: request.alpha,
        }),
    }
}

fn scale_frame_commands(commands: &[FrameCommand], scale: f32) -> Vec<FrameCommand> {
    if (scale - 1.0).abs() <= f32::EPSILON {
        return commands.to_vec();
    }
    commands
        .iter()
        .map(|command| scale_frame_command(command, scale))
        .collect()
}

fn process_input_thread_messages() -> bool {
    let mut attached_queue_ptr = {
        let state = app_state().lock().expect("android app state poisoned");
        state.attached_input_queue_ptr
    };
    let mut should_stop = false;
    while let Some(message) = {
        let mut state = app_state().lock().expect("android app state poisoned");
        state.input_thread_messages.pop_front()
    } {
        match message {
            InputThreadMessage::AttachQueue(queue_ptr) => {
                if let Some(existing_ptr) = attached_queue_ptr.take() {
                    unsafe {
                        ndk_sys::AInputQueue_detachLooper(
                            existing_ptr as *mut ndk_sys::AInputQueue,
                        );
                    }
                }
                let looper_ptr = {
                    let state = app_state().lock().expect("android app state poisoned");
                    state.input_looper_ptr
                };
                let Some(looper_ptr) = looper_ptr else {
                    let mut state = app_state().lock().expect("android app state poisoned");
                    state
                        .input_thread_messages
                        .push_front(InputThreadMessage::AttachQueue(queue_ptr));
                    break;
                };
                let queue = queue_ptr as *mut ndk_sys::AInputQueue;
                if !queue.is_null() {
                    unsafe {
                        ndk_sys::AInputQueue_attachLooper(
                            queue,
                            looper_ptr as *mut ndk_sys::ALooper,
                            INPUT_QUEUE_IDENT,
                            Some(on_input_queue_looper_event),
                            queue.cast(),
                        );
                    }
                    attached_queue_ptr = Some(queue_ptr);
                }
            }
            InputThreadMessage::DetachQueue => {
                if let Some(existing_ptr) = attached_queue_ptr.take() {
                    unsafe {
                        ndk_sys::AInputQueue_detachLooper(
                            existing_ptr as *mut ndk_sys::AInputQueue,
                        );
                    }
                }
            }
            InputThreadMessage::Stop => {
                should_stop = true;
                break;
            }
        }
    }
    let mut state = app_state().lock().expect("android app state poisoned");
    state.attached_input_queue_ptr = attached_queue_ptr;
    should_stop
}

fn start_input_thread_if_needed() -> bool {
    {
        let state = app_state().lock().expect("android app state poisoned");
        if state.input_thread_running {
            return true;
        }
    }

    {
        let mut state = app_state().lock().expect("android app state poisoned");
        state.input_thread_running = true;
        state.input_looper_ptr = None;
    }

    match std::thread::Builder::new().spawn(|| {
        let looper = unsafe {
            ndk_sys::ALooper_prepare(ndk_sys::ALOOPER_PREPARE_ALLOW_NON_CALLBACKS as i32)
        };
        if looper.is_null() {
            android_log_error("Android input thread failed: ALooper_prepare returned null");
            let mut state = app_state().lock().expect("android app state poisoned");
            state.input_thread_running = false;
            state.input_looper_ptr = None;
            return;
        }

        {
            let mut state = app_state().lock().expect("android app state poisoned");
            state.input_looper_ptr = Some(looper as usize);
        }
        android_log_info("Android dedicated input thread started");
        unsafe {
            ndk_sys::ALooper_wake(looper);
        }

        loop {
            if process_input_thread_messages() {
                break;
            }
            unsafe {
                ndk_sys::ALooper_pollOnce(
                    -1,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
            }
        }

        let mut state = app_state().lock().expect("android app state poisoned");
        state.input_thread_running = false;
        state.input_looper_ptr = None;
        state.attached_input_queue_ptr = None;
        android_log_info("Android dedicated input thread exited");
    }) {
        Ok(handle) => {
            std::mem::forget(handle);
            true
        }
        Err(err) => {
            let mut state = app_state().lock().expect("android app state poisoned");
            state.input_thread_running = false;
            state.input_looper_ptr = None;
            android_log_error(&format!(
                "Android dedicated input thread spawn failed: {err}"
            ));
            false
        }
    }
}

unsafe extern "C" fn on_input_queue_looper_event(_fd: i32, _events: i32, data: *mut c_void) -> i32 {
    let queue = data.cast::<ndk_sys::AInputQueue>();
    if !queue.is_null() {
        drain_input_queue(queue);
    }
    1
}

unsafe extern "C" fn on_frame_callback(_frame_time_nanos: i64, _data: *mut c_void) {
    {
        let mut state = app_state().lock().expect("android app state poisoned");
        state.frame_callback_scheduled = false;
    }
    pump_main_thread_reactor(true);
}

fn process_control_messages() -> (bool, bool) {
    let mut launched_future = false;
    let mut should_stop = false;
    let mut processed_message = false;
    while let Some(message) = {
        let mut state = app_state().lock().expect("android app state poisoned");
        state.control_messages.pop_front()
    } {
        processed_message = true;
        match message {
            ReactorMessage::LaunchFactory(factory) => {
                MAIN_THREAD_RUNTIME_FUTURE.with(|slot| {
                    *slot.borrow_mut() = Some(factory());
                });
                launched_future = true;
            }
            ReactorMessage::WindowCreated(window_ptr) => {
                let window = NonNull::new(window_ptr as *mut ndk_sys::ANativeWindow)
                    .map(|ptr| unsafe { NativeWindow::clone_from_ptr(ptr) });
                let mut state = app_state().lock().expect("android app state poisoned");
                state.window = window;
                if let Some(window) = state.window.as_ref() {
                    state.surface = logical_surface_info(window, state.display_scale);
                }
            }
            ReactorMessage::WindowDestroyed => {
                let mut state = app_state().lock().expect("android app state poisoned");
                state.window = None;
                state.surface = SurfaceInfo {
                    width: 0.0,
                    height: 0.0,
                };
                state.gles_backend = None;
            }
            ReactorMessage::InputQueueCreated(queue_ptr) => {
                if !start_input_thread_if_needed() {
                    android_log_error("Android input queue setup failed: input thread unavailable");
                } else {
                    let mut state = app_state().lock().expect("android app state poisoned");
                    state
                        .input_thread_messages
                        .push_back(InputThreadMessage::AttachQueue(queue_ptr));
                    drop(state);
                    wake_input_thread();
                }
            }
            ReactorMessage::InputQueueDestroyed => {
                let mut state = app_state().lock().expect("android app state poisoned");
                state
                    .input_thread_messages
                    .push_back(InputThreadMessage::DetachQueue);
                drop(state);
                wake_input_thread();
            }
            ReactorMessage::Stop => {
                let mut state = app_state().lock().expect("android app state poisoned");
                state
                    .input_thread_messages
                    .push_back(InputThreadMessage::Stop);
                drop(state);
                wake_input_thread();
                should_stop = true;
                break;
            }
        }
    }
    if processed_message {
        let mut state = app_state().lock().expect("android app state poisoned");
        state.event_epoch = state.event_epoch.saturating_add(1);
        wake_next_frame_waiters(&mut state);
    }
    (launched_future, should_stop)
}

fn pump_runtime(frame_tick: bool) {
    let runtime_future = MAIN_THREAD_RUNTIME_FUTURE.with(|slot| slot.borrow_mut().take());
    let Some(mut future) = runtime_future else {
        return;
    };
    if frame_tick {
        advance_frame_clock();
    }
    let completed = poll_entry_future(future.as_mut());
    {
        if completed {
            let mut state = app_state().lock().expect("android app state poisoned");
            state.runtime_completed = true;
            android_log_info("Android runtime future completed");
        } else {
            MAIN_THREAD_RUNTIME_FUTURE.with(|slot| {
                *slot.borrow_mut() = Some(future);
            });
        }
    }
    flush_queued_frame();
    if completed {
        request_activity_finish();
    }
}

fn pump_main_thread_reactor(frame_tick: bool) {
    if !ensure_main_thread_reactor_initialized() {
        return;
    }
    let (launched_future, should_stop) = process_control_messages();
    if should_stop {
        let mut state = app_state().lock().expect("android app state poisoned");
        state.reactor_running = false;
        state.looper_ptr = None;
        return;
    }
    pump_runtime(frame_tick || launched_future);
    let should_continue = {
        let state = app_state().lock().expect("android app state poisoned");
        state.runtime_started && !state.runtime_completed
    };
    let _ = should_continue;
}

fn requested_render_backend() -> DesktopRenderBackendKind {
    DesktopRenderBackendKind::Gles
}

fn unsupported_platform_detail() -> &'static str {
    "loadngo Android host is active"
}

fn describe_unsupported_gles_command(commands: &[FrameCommand]) -> Option<&'static str> {
    commands.iter().find_map(|command| match command {
        FrameCommand::Clear { .. } => None,
        FrameCommand::FillRect { .. } => None,
        FrameCommand::StrokeRect { .. } => None,
        FrameCommand::Image(_) => None,
        FrameCommand::Text(_) => Some("Text"),
        FrameCommand::Line { .. } => Some("Line"),
        FrameCommand::Circle { .. } => Some("Circle"),
        FrameCommand::Arc { .. } => None,
        FrameCommand::Polyline { .. } => Some("Polyline"),
        FrameCommand::ParticleBatch { .. } => Some("ParticleBatch"),
    })
}

const AMOTION_EVENT_ACTION_MASK: i32 = 0xff;

fn blank_snapshot() -> InputSnapshot {
    InputSnapshot {
        mouse_x: 0.0,
        mouse_y: 0.0,
        mouse_wheel_x: 0.0,
        mouse_wheel_y: 0.0,
        mouse_pressed: false,
        mouse_down: false,
        mouse_released: false,
        touches: [None; 8],
        escape_pressed: false,
        space_pressed: false,
        space_down: false,
        f3_pressed: false,
        r_pressed: false,
        up_pressed: false,
        down_pressed: false,
        modifiers: ui_core::Modifiers::default(),
        key_events: Vec::new(),
        keys_down: Vec::new(),
        typed_text: String::new(),
    }
}

fn approximate_text_metrics(text: &str, font_size: u16, font_scale: f32) -> TextMetrics {
    let glyphs = text.chars().count() as f32;
    TextMetrics {
        width: glyphs * font_size as f32 * font_scale * 0.6,
        height: font_size as f32 * font_scale,
    }
}

fn font_size_and_scale(size: f32) -> (u16, f32) {
    let clamped = size.max(1.0);
    let font_size = clamped.round().min(u16::MAX as f32) as u16;
    let font_scale = (clamped / font_size as f32).max(0.01);
    (font_size, font_scale)
}

fn effective_text_px(font_size: u16, font_scale: f32) -> f32 {
    #[cfg(target_os = "android")]
    {
        return (font_size as f32 * font_scale * 1.08).max(1.0);
    }

    #[cfg(not(target_os = "android"))]
    {
        (font_size as f32 * font_scale).max(1.0)
    }
}

#[derive(Clone, Copy)]
struct SoftwareTextLineLayout {
    px: f32,
    line_height: i32,
    baseline_offset: i32,
}

fn software_text_line_layout(
    font: Option<&SoftwareFont>,
    font_size: u16,
    font_scale: f32,
) -> SoftwareTextLineLayout {
    let px = effective_text_px(font_size, font_scale);
    if let Some(font) = font {
        if let Some(metrics) = font.inner.horizontal_line_metrics(px) {
            return SoftwareTextLineLayout {
                px,
                line_height: metrics.new_line_size.max(px).ceil() as i32,
                baseline_offset: metrics.ascent.ceil().max(0.0) as i32,
            };
        }
    }

    SoftwareTextLineLayout {
        px,
        line_height: px.ceil() as i32,
        baseline_offset: (px * 0.8).round() as i32,
    }
}

fn font_text_metrics(
    text: &str,
    font: Option<&DesktopFont>,
    font_size: u16,
    font_scale: f32,
) -> TextMetrics {
    let Some(font) = font.and_then(|font| font.software_font.as_ref()) else {
        return approximate_text_metrics(text, font_size, font_scale);
    };
    let mut hasher = DefaultHasher::new();
    (Arc::as_ptr(&font.inner) as usize).hash(&mut hasher);
    text.hash(&mut hasher);
    font_size.hash(&mut hasher);
    font_scale.to_bits().hash(&mut hasher);
    let cache_key = hasher.finish();
    if let Some(metrics) = text_metrics_cache()
        .lock()
        .expect("android text metrics cache poisoned")
        .get(&cache_key)
        .copied()
    {
        return metrics;
    }
    let layout = software_text_line_layout(Some(font), font_size, font_scale);
    let mut max_width = 0.0f32;
    let mut current_width = 0.0f32;
    let mut line_count = 1usize;
    for ch in text.chars() {
        if ch == '\n' {
            max_width = max_width.max(current_width);
            current_width = 0.0;
            line_count += 1;
            continue;
        }
        let metrics = font.inner.metrics(ch, layout.px);
        current_width += metrics.advance_width.max(metrics.width as f32);
    }
    max_width = max_width.max(current_width);
    let metrics = TextMetrics {
        width: max_width,
        height: layout.line_height.max(1) as f32 * line_count as f32,
    };
    let mut cache = text_metrics_cache()
        .lock()
        .expect("android text metrics cache poisoned");
    if cache.len() > 4096 {
        cache.clear();
    }
    cache.insert(cache_key, metrics);
    metrics
}

fn android_platform_font_candidates() -> &'static [&'static str] {
    &[
        "/system/fonts/NotoSans-Regular.ttf",
        "/system/fonts/Roboto-Regular.ttf",
        "/system/fonts/NotoSansCJK-Regular.ttc",
        "/system/fonts/NotoSansCJKjp-Regular.otf",
    ]
}

fn load_default_android_system_font() -> Option<SoftwareFont> {
    for candidate in android_platform_font_candidates() {
        let Ok(bytes) = std::fs::read(candidate) else {
            continue;
        };
        let Ok(font) =
            fontdue::Font::from_bytes(bytes.as_slice(), fontdue::FontSettings::default())
        else {
            continue;
        };
        android_log_info(&format!("Android default font loaded: {candidate}"));
        return Some(SoftwareFont {
            inner: Arc::new(font),
        });
    }
    None
}

fn ensure_default_font_loaded(state: &mut AndroidAppState) {
    if state.default_font.is_some() {
        return;
    }
    if let Some(font) = load_default_android_system_font() {
        state.default_font = Some(font.clone());
        if state.current_font.is_none() {
            state.current_font = Some(font);
        }
    }
}

async fn load_software_font_from_path(path: &str) -> Result<SoftwareFont, String> {
    let bytes = load_bytes(path).await?;
    fontdue::Font::from_bytes(bytes.as_slice(), fontdue::FontSettings::default())
        .map(|font| SoftwareFont {
            inner: Arc::new(font),
        })
        .map_err(|err| format!("failed to parse Android font {path}: {err}"))
}

fn current_asset_manager() -> Result<AssetManager, String> {
    let state = app_state().lock().expect("android app state poisoned");
    let Some(ptr) = state.asset_manager_ptr else {
        return Err("Android asset manager is unavailable".to_string());
    };
    let ptr = NonNull::new(ptr as *mut ndk_sys::AAssetManager)
        .expect("Android asset manager pointer should remain non-null");
    Ok(unsafe { AssetManager::from_ptr(ptr) })
}

fn runtime_assets_root() -> Result<PathBuf, String> {
    let state = app_state().lock().expect("android app state poisoned");
    state
        .internal_data_path
        .as_ref()
        .map(|path| path.join("loadngo-runtime"))
        .ok_or_else(|| "Android internal data path is unavailable".to_string())
}

fn runtime_writable_root() -> Result<PathBuf, String> {
    let state = app_state().lock().expect("android app state poisoned");
    state
        .internal_data_path
        .clone()
        .ok_or_else(|| "Android internal data path is unavailable".to_string())
}

fn extract_packaged_assets() -> Result<PathBuf, String> {
    let output_root = runtime_assets_root()?;
    let stamp_path = output_root.join(".assets-ready");
    let manifest_path = output_root.join("loadngo/assets/fonts/manifest.ron");

    if cfg!(debug_assertions) && output_root.exists() {
        std::fs::remove_dir_all(&output_root).map_err(|err| {
            format!(
                "failed to clear Android debug asset cache at {}: {err}",
                output_root.display()
            )
        })?;
    }

    if stamp_path.exists() && manifest_path.exists() {
        configure_runtime_env(&output_root);
        return Ok(output_root);
    }

    if output_root.exists() {
        std::fs::remove_dir_all(&output_root).map_err(|err| {
            format!(
                "failed to clear stale Android asset cache at {}: {err}",
                output_root.display()
            )
        })?;
    }

    std::fs::create_dir_all(&output_root)
        .map_err(|err| format!("failed to create Android runtime asset root: {err}"))?;
    let manager = current_asset_manager()?;
    extract_asset_subtree(&manager, "", &output_root)?;
    std::fs::write(&stamp_path, b"ok")
        .map_err(|err| format!("failed to write Android asset extraction stamp: {err}"))?;
    configure_runtime_env(&output_root);
    Ok(output_root)
}

fn configure_runtime_env(output_root: &Path) {
    if let Ok(writable_root) = runtime_writable_root() {
        unsafe {
            env::set_var("SNG_WRITABLE_ROOT", writable_root);
        }
    }
    unsafe {
        env::set_var("SNG_ASSETS_ROOT", output_root);
        env::set_var("LOADNGO_ASSETS_ROOT", output_root.join("loadngo/assets"));
    }
}

fn extract_asset_subtree(
    manager: &AssetManager,
    asset_rel: &str,
    output_root: &Path,
) -> Result<(), String> {
    let dir_name = CString::new(asset_rel)
        .map_err(|_| format!("invalid Android asset directory path: {asset_rel:?}"))?;
    let Some(mut dir) = manager.open_dir(dir_name.as_c_str()) else {
        return Ok(());
    };

    for entry in dir.by_ref() {
        let entry = entry
            .to_str()
            .map_err(|_| format!("Android asset name is not valid UTF-8 under {asset_rel:?}"))?;
        let child_rel = if asset_rel.is_empty() {
            entry.to_string()
        } else {
            format!("{asset_rel}/{entry}")
        };
        let child_cstr = CString::new(child_rel.as_str())
            .map_err(|_| format!("invalid Android asset path: {child_rel:?}"))?;
        if let Some(mut asset) = manager.open(child_cstr.as_c_str()) {
            let bytes = match asset.buffer() {
                Ok(buffer) => buffer.to_vec(),
                Err(_) => {
                    let mut bytes = Vec::new();
                    asset.read_to_end(&mut bytes).map_err(|err| {
                        format!("failed to read Android asset {child_rel}: {err}")
                    })?;
                    bytes
                }
            };
            let output_path = output_root.join(&child_rel);
            if let Some(parent) = output_path.parent() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    format!(
                        "failed to create parent directory for Android asset {}: {err}",
                        output_path.display()
                    )
                })?;
            }
            std::fs::write(&output_path, bytes).map_err(|err| {
                format!(
                    "failed to materialize Android asset {}: {err}",
                    output_path.display()
                )
            })?;
        } else {
            extract_asset_subtree(manager, &child_rel, output_root)?;
        }
    }

    Ok(())
}

fn normalize_asset_path(path: &str) -> String {
    let trimmed = path.trim_start_matches("./");
    if let Some(stripped) = trimmed.strip_prefix("../loadngo/assets/") {
        return format!("loadngo/assets/{stripped}");
    }
    trimmed
        .strip_prefix("assets/")
        .unwrap_or(trimmed)
        .to_string()
}

fn resolve_asset_rel_for_path(path: &str) -> String {
    let candidate = PathBuf::from(path);
    if let Ok(output_root) = runtime_assets_root() {
        if let Ok(stripped) = candidate.strip_prefix(&output_root) {
            return stripped.to_string_lossy().replace('\\', "/");
        }
    }
    normalize_asset_path(path)
}

fn materialize_asset_file(
    manager: &AssetManager,
    asset_rel: &str,
    output_path: &Path,
) -> Result<(), String> {
    let asset_name = CString::new(asset_rel)
        .map_err(|_| format!("invalid Android asset path: {asset_rel:?}"))?;
    let Some(mut asset) = manager.open(asset_name.as_c_str()) else {
        return Err(format!("Android asset not found: {asset_rel}"));
    };
    let bytes = match asset.buffer() {
        Ok(buffer) => buffer.to_vec(),
        Err(_) => {
            let mut bytes = Vec::new();
            asset
                .read_to_end(&mut bytes)
                .map_err(|err| format!("failed to read Android asset {asset_rel}: {err}"))?;
            bytes
        }
    };
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            format!(
                "failed to create parent directory for Android asset {}: {err}",
                output_path.display()
            )
        })?;
    }
    std::fs::write(output_path, bytes).map_err(|err| {
        format!(
            "failed to materialize Android asset {}: {err}",
            output_path.display()
        )
    })
}

pub(crate) fn ensure_materialized_asset_path(path: &str) -> Result<String, String> {
    let candidate = PathBuf::from(path);
    if candidate.exists() {
        return Ok(candidate.to_string_lossy().into_owned());
    }

    let output_root = runtime_assets_root()?;
    let asset_rel = if let Ok(stripped) = candidate.strip_prefix(&output_root) {
        stripped.to_string_lossy().replace('\\', "/")
    } else {
        normalize_asset_path(path)
    };
    let output_path = output_root.join(&asset_rel);
    if output_path.exists() {
        return Ok(output_path.to_string_lossy().into_owned());
    }

    let manager = current_asset_manager()?;
    materialize_asset_file(&manager, &asset_rel, &output_path)?;
    Ok(output_path.to_string_lossy().into_owned())
}

pub fn asset_exists(path: &str) -> bool {
    let candidate = PathBuf::from(path);
    if candidate.exists() {
        return true;
    }

    let Ok(manager) = current_asset_manager() else {
        return false;
    };
    let asset_rel = resolve_asset_rel_for_path(path);
    let Ok(asset_name) = CString::new(asset_rel.as_str()) else {
        return false;
    };
    manager.open(asset_name.as_c_str()).is_some()
}

pub fn set_text_cursor_active(_active: bool) {}

pub fn read_clipboard_text() -> Result<Option<String>, String> {
    Ok(None)
}

pub fn write_clipboard_text(_text: &str) -> Result<(), String> {
    Ok(())
}

pub fn desktop_render_backend_status() -> DesktopRenderBackendStatus {
    let state = app_state().lock().expect("android app state poisoned");
    let gles_state = state
        .gles_backend
        .as_ref()
        .map(GlesBackend::state)
        .unwrap_or(GlesBackendState::UnboundSurface);
    DesktopRenderBackendStatus {
        requested: requested_render_backend(),
        last_used: state.last_backend_used,
        metal_initialized: matches!(
            gles_state,
            GlesBackendState::Ready | GlesBackendState::SurfaceBound
        ),
        metal_surface_bound: matches!(gles_state, GlesBackendState::SurfaceBound),
        detail: if state.backend_detail.is_empty() {
            unsupported_platform_detail().to_string()
        } else {
            state.backend_detail.clone()
        },
    }
}

pub fn register_android_runtime_entry(entry: fn()) {
    let mut state = app_state().lock().expect("android app state poisoned");
    state.runtime_entry = Some(entry);
}

pub unsafe fn android_native_activity_on_create(
    activity: *mut c_void,
    _saved_state: *mut c_void,
    _saved_state_size: usize,
) {
    android_log_info("ANativeActivity_onCreate entered");
    let Some(activity_ptr) = NonNull::new(activity.cast::<ndk_sys::ANativeActivity>()) else {
        android_log_error("ANativeActivity_onCreate received null activity");
        return;
    };

    let asset_manager_ptr = unsafe {
        NonNull::new(activity_ptr.as_ref().assetManager)
            .expect("ANativeActivity asset manager should be present")
    };
    let internal_data_path = unsafe {
        let raw = activity_ptr.as_ref().internalDataPath;
        let cstr = std::ffi::CStr::from_ptr(raw);
        PathBuf::from(
            cstr.to_str()
                .expect("Android internal data path should be valid UTF-8"),
        )
    };

    unsafe {
        ndk_context::initialize_android_context(
            activity_ptr.as_ref().vm.cast(),
            activity_ptr.as_ref().clazz.cast(),
        );
    }

    unsafe {
        let callbacks = activity_ptr
            .as_ref()
            .callbacks
            .as_mut()
            .expect("ANativeActivity callbacks should be present");
        callbacks.onNativeWindowCreated = Some(on_native_window_created);
        callbacks.onNativeWindowDestroyed = Some(on_native_window_destroyed);
        callbacks.onInputQueueCreated = Some(on_input_queue_created);
        callbacks.onInputQueueDestroyed = Some(on_input_queue_destroyed);
        callbacks.onPause = Some(on_pause);
        callbacks.onResume = Some(on_resume);
        callbacks.onWindowFocusChanged = Some(on_window_focus_changed);
        callbacks.onDestroy = Some(on_destroy);
    }

    {
        let mut state = app_state().lock().expect("android app state poisoned");
        let display_scale = android_display_scale(Some(asset_manager_ptr.as_ptr() as usize));
        state.activity_ptr = Some(activity_ptr.as_ptr() as usize);
        state.asset_manager_ptr = Some(asset_manager_ptr.as_ptr() as usize);
        state.internal_data_path = Some(internal_data_path);
        state.display_scale = display_scale;
        android_log_info(&format!(
            "Android display scale initialized to {:.3}",
            display_scale
        ));
    }
    if let Err(err) = extract_packaged_assets() {
        eprintln!("Android asset extraction failed: {err}");
        android_log_error(&format!("Android asset extraction failed: {err}"));
    } else {
        android_log_info("Android assets extracted and environment configured");
    }
}

unsafe extern "C" fn on_native_window_created(
    _activity: *mut ndk_sys::ANativeActivity,
    window: *mut ndk_sys::ANativeWindow,
) {
    {
        let mut state = app_state().lock().expect("android app state poisoned");
        state.window_ptr = NonNull::new(window).map(|ptr| ptr.as_ptr() as usize);
        if let Some(window_ptr) = state.window_ptr {
            if state.reactor_running {
                state
                    .control_messages
                    .push_back(ReactorMessage::WindowCreated(window_ptr));
            }
        }
    }
    pump_main_thread_reactor(false);
    let runtime_entry = {
        let mut state = app_state().lock().expect("android app state poisoned");
        if state.runtime_started || state.runtime_completed {
            None
        } else {
            state.runtime_entry.take()
        }
    };
    android_log_info("Android native window created");
    if let Some(entry) = runtime_entry {
        android_log_info("Android runtime entering on native activity thread");
        entry();
        android_log_info("Android runtime returned on native activity thread");
    }
}

unsafe extern "C" fn on_native_window_destroyed(
    _activity: *mut ndk_sys::ANativeActivity,
    _window: *mut ndk_sys::ANativeWindow,
) {
    let mut state = app_state().lock().expect("android app state poisoned");
    state.window_ptr = None;
    state.window = None;
    state.surface = SurfaceInfo {
        width: 0.0,
        height: 0.0,
    };
    state.gles_backend = None;
    if state.reactor_running {
        state
            .control_messages
            .push_back(ReactorMessage::WindowDestroyed);
    }
    drop(state);
    pump_main_thread_reactor(false);
    android_log_info("Android native window destroyed");
}

unsafe extern "C" fn on_input_queue_created(
    _activity: *mut ndk_sys::ANativeActivity,
    queue: *mut ndk_sys::AInputQueue,
) {
    let mut state = app_state().lock().expect("android app state poisoned");
    state.input_queue_ptr = NonNull::new(queue).map(|ptr| ptr.as_ptr() as usize);
    if state.reactor_running {
        if let Some(queue_ptr) = state.input_queue_ptr {
            state
                .control_messages
                .push_back(ReactorMessage::InputQueueCreated(queue_ptr));
        }
    }
    drop(state);
    pump_main_thread_reactor(false);
    android_log_info("Android input queue created");
}

unsafe extern "C" fn on_input_queue_destroyed(
    _activity: *mut ndk_sys::ANativeActivity,
    _queue: *mut ndk_sys::AInputQueue,
) {
    let mut state = app_state().lock().expect("android app state poisoned");
    state.input_queue_ptr = None;
    state.input.touches = [None; 8];
    state.input.mouse_down = false;
    if state.reactor_running {
        state
            .control_messages
            .push_back(ReactorMessage::InputQueueDestroyed);
    }
    drop(state);
    pump_main_thread_reactor(false);
    android_log_info("Android input queue destroyed");
}

/// Android's own signal that the activity has stopped being the interactive,
/// visible foreground — screen lock, an incoming call, the recents overlay,
/// another app covering it, and so on all reach here. Unlike
/// `on_native_window_destroyed`, this reliably fires immediately on a plain
/// screen lock (the native window itself is often kept alive across a lock),
/// which is exactly the case that let audio (and simulation time) keep
/// running silently off-screen before this callback existed.
unsafe extern "C" fn on_pause(_activity: *mut ndk_sys::ANativeActivity) {
    let mut state = app_state().lock().expect("android app state poisoned");
    state.foreground = false;
    drop(state);
    android_log_info("Android activity paused (backgrounded)");
}

unsafe extern "C" fn on_resume(_activity: *mut ndk_sys::ANativeActivity) {
    let mut state = app_state().lock().expect("android app state poisoned");
    state.foreground = true;
    drop(state);
    android_log_info("Android activity resumed (foregrounded)");
    refresh_system_ui();
}

/// Android clears sticky-immersive `setSystemUiVisibility` flags whenever
/// the window regains focus (returning from recents, unlocking, a dialog
/// closing), so they need reapplying here in addition to `on_resume` — the
/// two fire at different, only-partially-overlapping moments. Real
/// safe-area insets are refreshed at both points too, since a focus-regain
/// is exactly when the previous query (taken while immersive flags may have
/// been transiently cleared) could be stale.
unsafe extern "C" fn on_window_focus_changed(
    _activity: *mut ndk_sys::ANativeActivity,
    has_focus: std::os::raw::c_int,
) {
    if has_focus != 0 {
        refresh_system_ui();
    }
}

/// Reapplies immersive mode if the game has requested it, then re-queries
/// real safe-area insets — in that order, since hiding the system bars
/// changes what `getSystemWindowInset*` reports (a hidden bar claims no
/// space), and the insets the game actually cares about are whatever is
/// true once immersive presentation has taken effect.
fn refresh_system_ui() {
    let immersive_requested = app_state()
        .lock()
        .expect("android app state poisoned")
        .immersive_requested;
    if immersive_requested {
        apply_immersive_mode();
    }
    let insets = query_safe_area_insets();
    app_state()
        .lock()
        .expect("android app state poisoned")
        .insets = insets;
}

// `View.SYSTEM_UI_FLAG_*` bit values, stable public API since API 19.
// Named and OR'd explicitly (rather than hand-computed into one hex
// literal, and rather than looked up via JNI static fields to avoid six
// extra round-trips for values that don't change) specifically so this
// can't silently drift into an invalid combination again — a prior version
// of this constant included both `IMMERSIVE` and `IMMERSIVE_STICKY`
// simultaneously (they're mutually exclusive per Android's own docs),
// which left the navigation bar visible on-device despite the status bar
// correctly hiding.
const SYSTEM_UI_FLAG_HIDE_NAVIGATION: i32 = 0x0000_0002;
const SYSTEM_UI_FLAG_FULLSCREEN: i32 = 0x0000_0004;
const SYSTEM_UI_FLAG_LAYOUT_STABLE: i32 = 0x0000_0100;
const SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION: i32 = 0x0000_0200;
const SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN: i32 = 0x0000_0400;
const SYSTEM_UI_FLAG_IMMERSIVE_STICKY: i32 = 0x0000_1000;

/// "Sticky immersive" presentation: hidden status and navigation bars,
/// swipe-to-reveal, flags reapplied automatically rather than a one-shot
/// reveal.
const IMMERSIVE_STICKY_FLAGS: i32 = SYSTEM_UI_FLAG_LAYOUT_STABLE
    | SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
    | SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
    | SYSTEM_UI_FLAG_HIDE_NAVIGATION
    | SYSTEM_UI_FLAG_FULLSCREEN
    | SYSTEM_UI_FLAG_IMMERSIVE_STICKY;

/// `Activity.getWindow().getDecorView()`, the shared first step for
/// immersive mode, insets, and gesture-exclusion rects. `None` if the
/// window isn't attached yet (both steps are nullable in principle).
fn decor_view<'e>(env: &mut jni::JNIEnv<'e>) -> Result<Option<JObject<'e>>, String> {
    let ctx = ndk_context::android_context();
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let Some(window) =
        android_jni::call_object(env, &activity, "getWindow", "()Landroid/view/Window;", &[])?
    else {
        return Ok(None);
    };
    android_jni::call_object(env, &window, "getDecorView", "()Landroid/view/View;", &[])
}

/// API 30+ where it's available: apps targeting `targetSdkVersion` 30+ get
/// the legacy `setSystemUiVisibility` navigation-bar-hide flag silently
/// ignored by the framework on real Android 11+ devices — confirmed
/// on-device (Android 14 / API 34): the status bar hid correctly but the
/// navigation bar stayed visible with `setSystemUiVisibility` alone.
/// `WindowInsetsController` is the framework's own replacement and is the
/// only reliable way to hide both bars at this crate's `targetSdkVersion`.
const WINDOW_INSETS_CONTROLLER_MIN_SDK: i32 = 30;

fn apply_immersive_mode() {
    let result = android_jni::with_env(|env| {
        let Some(decor_view) = decor_view(env)? else {
            return Err("Window.getDecorView() unavailable".to_string());
        };
        let sdk_int =
            android_jni::get_static_int_field(env, "android/os/Build$VERSION", "SDK_INT")?;
        if sdk_int >= WINDOW_INSETS_CONTROLLER_MIN_SDK {
            apply_immersive_mode_via_insets_controller(env, &decor_view)
        } else {
            android_jni::call_void(
                env,
                &decor_view,
                "setSystemUiVisibility",
                "(I)V",
                &[JValue::Int(IMMERSIVE_STICKY_FLAGS)],
            )
        }
    });
    if let Err(err) = result {
        android_log_error(&format!("Android immersive mode request failed: {err}"));
    }
}

fn apply_immersive_mode_via_insets_controller(
    env: &mut jni::JNIEnv,
    decor_view: &JObject,
) -> Result<(), String> {
    let Some(controller) = android_jni::call_object(
        env,
        decor_view,
        "getWindowInsetsController",
        "()Landroid/view/WindowInsetsController;",
        &[],
    )?
    else {
        return Err("View.getWindowInsetsController() returned null".to_string());
    };
    let system_bars_type = android_jni::call_static_int(
        env,
        "android/view/WindowInsets$Type",
        "systemBars",
        "()I",
        &[],
    )?;
    android_jni::call_void(
        env,
        &controller,
        "hide",
        "(I)V",
        &[JValue::Int(system_bars_type)],
    )?;
    let behavior_swipe = android_jni::get_static_int_field(
        env,
        "android/view/WindowInsetsController",
        "BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE",
    )?;
    android_jni::call_void(
        env,
        &controller,
        "setSystemBarsBehavior",
        "(I)V",
        &[JValue::Int(behavior_swipe)],
    )
}

/// Requests (or releases) sticky-immersive presentation: hidden status and
/// navigation bars, swipe-to-reveal. This is a level, not a one-shot event —
/// `loadngo` remembers the request and reapplies it itself on `on_resume`
/// and `on_window_focus_changed`, since Android clears the flags on its own
/// whenever the window regains focus. Call once; do not call every frame.
pub fn set_immersive_mode(enabled: bool) {
    let mut state = app_state().lock().expect("android app state poisoned");
    state.immersive_requested = enabled;
    drop(state);
    if enabled {
        apply_immersive_mode();
    }
}

fn system_bar_insets_via_type(
    env: &mut jni::JNIEnv,
    root_insets: &JObject,
) -> Result<SafeAreaInsets, String> {
    let system_bars_type = android_jni::call_static_int(
        env,
        "android/view/WindowInsets$Type",
        "systemBars",
        "()I",
        &[],
    )?;
    let Some(insets_obj) = android_jni::call_object(
        env,
        root_insets,
        "getInsets",
        "(I)Landroid/graphics/Insets;",
        &[JValue::Int(system_bars_type)],
    )?
    else {
        return Ok(SafeAreaInsets::default());
    };
    Ok(SafeAreaInsets {
        left: android_jni::get_int_field(env, &insets_obj, "left")? as f32,
        top: android_jni::get_int_field(env, &insets_obj, "top")? as f32,
        right: android_jni::get_int_field(env, &insets_obj, "right")? as f32,
        bottom: android_jni::get_int_field(env, &insets_obj, "bottom")? as f32,
    })
}

fn system_bar_insets_via_legacy_methods(
    env: &mut jni::JNIEnv,
    root_insets: &JObject,
) -> Result<SafeAreaInsets, String> {
    Ok(SafeAreaInsets {
        left: android_jni::call_int(env, root_insets, "getSystemWindowInsetLeft", "()I", &[])?
            as f32,
        top: android_jni::call_int(env, root_insets, "getSystemWindowInsetTop", "()I", &[])? as f32,
        right: android_jni::call_int(env, root_insets, "getSystemWindowInsetRight", "()I", &[])?
            as f32,
        bottom: android_jni::call_int(env, root_insets, "getSystemWindowInsetBottom", "()I", &[])?
            as f32,
    })
}

/// Real device-reserved screen space: system-bar insets only. Deliberately
/// excludes the display cutout: a cutout notch sits at a specific point
/// along an edge (usually near a device's short-edge center, wherever the
/// front camera is), but a blanket cutout-avoidance margin is correct only
/// for content placed right where the cutout is, and wildly
/// over-conservative for content anchored somewhere else on that same
/// edge. System bars don't have this problem — they genuinely span the
/// whole edge — so only they're reflected here. A future top-anchored
/// element that actually needs cutout-awareness should query
/// `DisplayCutout`'s own bounding rects directly rather than this blanket
/// scalar.
///
/// On `SDK_INT >= 30`, `WindowInsets.getInsets(WindowInsets.Type.systemBars())`
/// is used — the modern, precisely-scoped API that reports only system-bar
/// space. Below that, this falls back to the legacy
/// `getSystemWindowInset{Left,Top,Right,Bottom}()` methods, which on-device
/// verification showed do *not* cleanly separate system bars from the
/// cutout on at least one real API 34 device (`getSystemWindowInsetLeft()`
/// returned the same value as the cutout's own `getSafeInsetLeft()`, not a
/// bars-only figure) — so the legacy path still carries some of the
/// original over-conservative-corner problem below API 30. Revisit if that
/// turns out to matter in practice on an API 26-29 device.
/// Returns all-zero (not an error) if the window isn't attached yet or any
/// step fails — callers already treat zero as "no better data available."
fn query_safe_area_insets() -> SafeAreaInsets {
    let result = android_jni::with_env(|env| {
        let Some(decor_view) = decor_view(env)? else {
            return Ok(SafeAreaInsets::default());
        };
        let Some(root_insets) = android_jni::call_object(
            env,
            &decor_view,
            "getRootWindowInsets",
            "()Landroid/view/WindowInsets;",
            &[],
        )?
        else {
            return Ok(SafeAreaInsets::default());
        };
        let sdk_int =
            android_jni::get_static_int_field(env, "android/os/Build$VERSION", "SDK_INT")?;
        if sdk_int >= WINDOW_INSETS_CONTROLLER_MIN_SDK {
            system_bar_insets_via_type(env, &root_insets)
        } else {
            system_bar_insets_via_legacy_methods(env, &root_insets)
        }
    });
    match result {
        Ok(insets) => {
            android_log_info(&format!(
                "Android safe-area insets queried: left={} top={} right={} bottom={}",
                insets.left, insets.top, insets.right, insets.bottom
            ));
            insets
        }
        Err(err) => {
            android_log_error(&format!("Android safe-area insets query failed: {err}"));
            SafeAreaInsets::default()
        }
    }
}

/// Tells the platform to prioritize the app's own touch handling over a
/// competing system gesture (Android's edge-swipe back gesture, in
/// practice) within the given screen-space rects — the natural fit for
/// persistent on-screen controls that sit in the back-swipe zone. No-op
/// below `SDK_INT` 29, where the underlying method doesn't exist.
pub fn set_gesture_exclusion_rects(rects: &[ExclusionRect]) {
    let result = android_jni::with_env(|env| {
        let sdk_int =
            android_jni::get_static_int_field(env, "android/os/Build$VERSION", "SDK_INT")?;
        if sdk_int < 29 {
            return Ok(());
        }
        let Some(decor_view) = decor_view(env)? else {
            return Err("Window.getDecorView() unavailable".to_string());
        };
        let list = env
            .new_object("java/util/ArrayList", "()V", &[])
            .map_err(|err| format!("Failed to allocate ArrayList: {err}"))?;
        for rect in rects {
            let android_rect = env
                .new_object(
                    "android/graphics/Rect",
                    "(IIII)V",
                    &[
                        JValue::Int(rect.x.round() as i32),
                        JValue::Int(rect.y.round() as i32),
                        JValue::Int((rect.x + rect.width).round() as i32),
                        JValue::Int((rect.y + rect.height).round() as i32),
                    ],
                )
                .map_err(|err| format!("Failed to allocate Rect: {err}"))?;
            android_jni::call_bool(
                env,
                &list,
                "add",
                "(Ljava/lang/Object;)Z",
                &[JValue::Object(&android_rect)],
            )?;
        }
        android_jni::call_void(
            env,
            &decor_view,
            "setSystemGestureExclusionRects",
            "(Ljava/util/List;)V",
            &[JValue::Object(&list)],
        )
    });
    if let Err(err) = result {
        android_log_error(&format!(
            "Android gesture-exclusion rects request failed: {err}"
        ));
    }
}

unsafe extern "C" fn on_destroy(_activity: *mut ndk_sys::ANativeActivity) {
    let mut state = app_state().lock().expect("android app state poisoned");
    state.window = None;
    state.input_queue_ptr = None;
    state.runtime_completed = true;
    state.gles_backend = None;
    state.control_messages.push_back(ReactorMessage::Stop);
    drop(state);
    pump_main_thread_reactor(false);
    android_log_info("Android native activity destroyed");
    unsafe {
        ndk_context::release_android_context();
    }
}

pub fn launch(
    _window: WindowDescriptor,
    _icon: Option<WindowIconSet>,
    entry: impl Future<Output = ()> + Send + 'static,
) {
    let mut state = app_state().lock().expect("android app state poisoned");
    if state.runtime_started {
        android_log_info("Android runtime launch skipped because it already started");
        return;
    }
    state.runtime_started = true;
    state
        .control_messages
        .push_back(ReactorMessage::LaunchFactory(Box::new(move || {
            Box::pin(entry)
        })));
    drop(state);
    pump_main_thread_reactor(false);
    android_log_info("Android runtime loop starting");
}

pub fn launch_with_factory<E, F>(
    _window: WindowDescriptor,
    _icon: Option<WindowIconSet>,
    entry_factory: E,
) where
    E: FnOnce() -> F + Send + 'static,
    F: Future<Output = ()> + 'static,
{
    let mut state = app_state().lock().expect("android app state poisoned");
    if state.runtime_started {
        android_log_info("Android runtime launch skipped because it already started");
        return;
    }
    state.runtime_started = true;
    state
        .control_messages
        .push_back(ReactorMessage::LaunchFactory(Box::new(move || {
            Box::pin(entry_factory())
        })));
    drop(state);
    pump_main_thread_reactor(false);
    android_log_info("Android runtime loop starting from entry factory");
}

fn renderer() -> Renderer {
    Renderer::new(RendererConfig::default())
}

fn queue_commands(commands: impl IntoIterator<Item = FrameCommand>) {
    let mut state = app_state().lock().expect("android app state poisoned");
    state.queued_commands.extend(commands);
}

fn flush_queued_frame() {
    let flush_started = Instant::now();
    let (window, commands, textures, generated_cache, current_font, display_scale) = {
        let mut state = app_state().lock().expect("android app state poisoned");
        ensure_default_font_loaded(&mut state);
        let Some(window) = state.window.clone() else {
            return;
        };
        if state.queued_commands.is_empty() {
            return;
        }
        let current_font = state
            .current_font
            .clone()
            .or_else(|| state.default_font.clone());
        if let Some(font) = current_font.as_ref() {
            let font_id = Arc::as_ptr(&font.inner) as usize;
            if state.generated_texture_cache_font_id != Some(font_id) {
                state.generated_texture_cache.clear();
                state.generated_texture_cache_font_id = Some(font_id);
            }
        }
        (
            window,
            std::mem::take(&mut state.queued_commands),
            state.texture_registry.clone(),
            std::mem::take(&mut state.generated_texture_cache),
            current_font,
            state.display_scale,
        )
    };
    let commands = scale_frame_commands(&commands, display_scale);

    let prepare_started = Instant::now();
    let (gles_commands, gles_textures, generated_cache) =
        prepare_gles_frame(&commands, &textures, generated_cache, current_font.as_ref());
    let prepare_elapsed = prepare_started.elapsed();
    {
        let mut state = app_state().lock().expect("android app state poisoned");
        state.generated_texture_cache = generated_cache;
    }

    let requested = requested_render_backend();
    let mut rendered = false;
    let mut render_elapsed = Duration::ZERO;
    let mut backend_used = "none";

    if requested == DesktopRenderBackendKind::Gles {
        let gles_result = {
            let mut state = app_state().lock().expect("android app state poisoned");
            if state.gles_backend.is_none() {
                android_log_info(&format!(
                    "Android GLES backend attempting native-window bind {}x{}",
                    window.width(),
                    window.height()
                ));
                match GlesBackend::try_bind_native_window(&window) {
                    Ok(backend) => {
                        state.gles_backend = Some(backend);
                        update_backend_detail(
                            &mut state,
                            "Android GLES backend bound to the native window",
                        );
                    }
                    Err(err) => {
                        update_backend_detail(
                            &mut state,
                            format!("Android GLES backend unavailable: {err}"),
                        );
                    }
                }
            }

            if let Some(backend) = state.gles_backend.as_mut() {
                backend.update_surface_size(window.width(), window.height());
                backend.sync_image_resources(
                    gles_textures
                        .iter()
                        .map(|(key, texture)| (key.clone(), texture.to_gles_resource())),
                );
                if backend.supports_commands(&gles_commands) {
                    let render_started = Instant::now();
                    let result = loadngo_renderer::Renderer::new(RendererConfig::default())
                        .render(backend, &gles_commands);
                    render_elapsed = render_started.elapsed();
                    backend_used = "gles";
                    match result {
                        Ok(()) => {
                            state.last_backend_used = DesktopRenderBackendKind::Gles;
                            update_backend_detail(
                                &mut state,
                                "Android GLES backend rendered the queued frame",
                            );
                            Some(Ok(()))
                        }
                        Err(err) => {
                            update_backend_detail(
                                &mut state,
                                format!("Android GLES backend render failed: {err}"),
                            );
                            Some(Err(err))
                        }
                    }
                } else {
                    let unsupported =
                        describe_unsupported_gles_command(&gles_commands).unwrap_or("Unknown");
                    update_backend_detail(
                        &mut state,
                        format!(
                            "Android GLES backend rejected queued frame: unsupported command {unsupported}"
                        ),
                    );
                    state.gles_backend = None;
                    None
                }
            } else {
                None
            }
        };

        match gles_result {
            Some(Ok(())) => rendered = true,
            Some(Err(err)) => android_log_error(&format!("Android GLES render failed: {err}")),
            None => {}
        }
    }

    if !rendered {
        if requested == DesktopRenderBackendKind::Gles {
            let mut state = app_state().lock().expect("android app state poisoned");
            if state.backend_detail.is_empty() {
                update_backend_detail(
                    &mut state,
                    "Android GLES backend did not render the queued frame",
                );
            }
            return;
        }

        if let Err(err) = software_present(&window, &commands, &textures, current_font.as_ref()) {
            android_log_error(&format!("Android software present failed: {err}"));
            let mut state = app_state().lock().expect("android app state poisoned");
            update_backend_detail(
                &mut state,
                format!("Android software renderer failed: {err}"),
            );
            return;
        }

        let mut state = app_state().lock().expect("android app state poisoned");
        state.last_backend_used = DesktopRenderBackendKind::Software;
        update_backend_detail(
            &mut state,
            "Android software renderer rendered the queued frame",
        );
        backend_used = "software";
    }

    let mut state = app_state().lock().expect("android app state poisoned");
    state.presented_frames = state.presented_frames.saturating_add(1);
    if state.presented_frames == 1 {
        let message = match state.last_backend_used {
            DesktopRenderBackendKind::Gles => "Android GLES backend posted the first frame",
            DesktopRenderBackendKind::Software => {
                "Android software renderer posted the first frame"
            }
            DesktopRenderBackendKind::Unavailable => "Android renderer posted the first frame",
        };
        android_log_info(message);
    }

    let total_elapsed = flush_started.elapsed();
    if total_elapsed >= Duration::from_millis(8) {
        android_log_info(&format!(
            "Android frame flush total={}ms prepare={}ms render={}ms backend={} commands={} gles_commands={} presented_frames={}",
            total_elapsed.as_millis(),
            prepare_elapsed.as_millis(),
            render_elapsed.as_millis(),
            backend_used,
            commands.len(),
            gles_commands.len(),
            state.presented_frames,
        ));
    }
}

fn prepare_gles_frame(
    commands: &[FrameCommand],
    textures: &HashMap<String, SoftwareTexture>,
    mut generated_cache: HashMap<String, SoftwareTexture>,
    current_font: Option<&SoftwareFont>,
) -> (
    Vec<FrameCommand>,
    HashMap<String, SoftwareTexture>,
    HashMap<String, SoftwareTexture>,
) {
    let mut next_commands = Vec::with_capacity(commands.len());
    let mut next_textures = textures.clone();
    let mut generated_index = 0usize;

    for command in commands {
        match command {
            FrameCommand::Text(request) => {
                if let Some(font) = current_font {
                    let image_key = generated_text_cache_key(request, font);
                    if !generated_cache.contains_key(&image_key) {
                        if let Some(texture) = rasterize_text_command(request, font) {
                            generated_cache.insert(image_key.clone(), texture);
                        }
                    }
                    if let Some(texture) = generated_cache.get(&image_key) {
                        next_textures.insert(image_key.clone(), texture.clone());
                        next_commands.push(FrameCommand::Image(ImageRequest {
                            rect: UiRect {
                                x: request.rect.x,
                                y: request.rect.y,
                                width: texture.width as f32,
                                height: texture.height as f32,
                            },
                            clip_rect: Some(request.rect),
                            image_key,
                            alpha: 1.0,
                        }));
                        continue;
                    }
                }
                next_commands.push(command.clone());
            }
            FrameCommand::Line { .. } => next_commands.push(command.clone()),
            FrameCommand::Circle { .. } => next_commands.push(command.clone()),
            FrameCommand::Arc { .. } => next_commands.push(command.clone()),
            FrameCommand::Polyline { .. } => next_commands.push(command.clone()),
            FrameCommand::ParticleBatch { particles } => append_rasterized_particle_textures(
                &mut next_commands,
                &mut next_textures,
                particles,
                &mut generated_index,
            ),
            _ => next_commands.push(command.clone()),
        }
    }

    (next_commands, next_textures, generated_cache)
}

fn generated_text_cache_key(
    request: &loadngo_renderer::TextRequest,
    font: &SoftwareFont,
) -> String {
    let mut hasher = DefaultHasher::new();
    (Arc::as_ptr(&font.inner) as usize).hash(&mut hasher);
    request.text.hash(&mut hasher);
    request.rect.width.to_bits().hash(&mut hasher);
    request.rect.height.to_bits().hash(&mut hasher);
    request.style.font_size.hash(&mut hasher);
    request.style.horizontal_align.hash(&mut hasher);
    request.style.vertical_align.hash(&mut hasher);
    request.style.layout_mode.hash(&mut hasher);
    request.style.overflow.hash(&mut hasher);
    request.style.color.r.hash(&mut hasher);
    request.style.color.g.hash(&mut hasher);
    request.style.color.b.hash(&mut hasher);
    request.style.color.a.hash(&mut hasher);
    format!("generated://text/{:016x}", hasher.finish())
}

fn rasterize_text_command(
    request: &loadngo_renderer::TextRequest,
    font: &SoftwareFont,
) -> Option<SoftwareTexture> {
    let measured = font_text_metrics(
        &request.text,
        Some(&DesktopFont {
            source_path: None,
            software_font: Some(font.clone()),
        }),
        request.style.font_size,
        1.0,
    );
    let padding_x = 4.0;
    let padding_y = 6.0;
    let line_box_height = single_line_text_box_height(request.style.font_size);
    let width = request
        .rect
        .width
        .max(measured.width.ceil() + padding_x * 2.0)
        .max(1.0);
    let height = request
        .rect
        .height
        .max(line_box_height + padding_y * 2.0)
        .max(measured.height.ceil() + padding_y * 2.0)
        .max(1.0);
    let mut surface = OwnedSoftwareSurface::new(width.ceil() as usize, height.ceil() as usize);
    let mut local_request = request.clone();
    local_request.rect = UiRect {
        x: padding_x,
        y: padding_y,
        width: (width - padding_x * 2.0).max(1.0),
        height: (height - padding_y * 2.0).max(1.0),
    };
    surface.draw_text(&local_request, Some(font));
    Some(surface.into_texture())
}

fn rasterize_line_command(
    from: ui_core::geometry::Point,
    to: ui_core::geometry::Point,
    color: UiColor,
    thickness: i32,
    index: usize,
) -> Option<(String, UiRect, SoftwareTexture)> {
    let thickness = thickness.max(1);
    let thickness_f = thickness as f32;
    let min_x = from.x.min(to.x) - thickness_f;
    let min_y = from.y.min(to.y) - thickness_f;
    let max_x = from.x.max(to.x) + thickness_f;
    let max_y = from.y.max(to.y) + thickness_f;
    let rect = UiRect {
        x: min_x,
        y: min_y,
        width: (max_x - min_x).max(1.0),
        height: (max_y - min_y).max(1.0),
    };
    let mut surface =
        OwnedSoftwareSurface::new(rect.width.ceil() as usize, rect.height.ceil() as usize);
    surface.line(
        ui_core::geometry::Point {
            x: from.x - rect.x,
            y: from.y - rect.y,
        },
        ui_core::geometry::Point {
            x: to.x - rect.x,
            y: to.y - rect.y,
        },
        color,
        thickness,
    );
    Some((
        format!("generated://line/{index}"),
        rect,
        surface.into_texture(),
    ))
}

fn rasterize_circle_command(
    center: ui_core::geometry::Point,
    radius: f32,
    color: UiColor,
    index: usize,
) -> Option<(String, UiRect, SoftwareTexture)> {
    if radius <= 0.0 {
        return None;
    }
    let rect = UiRect {
        x: center.x - radius,
        y: center.y - radius,
        width: radius * 2.0,
        height: radius * 2.0,
    };
    let mut surface = OwnedSoftwareSurface::new(
        rect.width.max(1.0).ceil() as usize,
        rect.height.max(1.0).ceil() as usize,
    );
    surface.circle(radius, radius, radius, color);
    Some((
        format!("generated://circle/{index}"),
        rect,
        surface.into_texture(),
    ))
}

fn append_rasterized_polyline_textures(
    commands: &mut Vec<FrameCommand>,
    textures: &mut HashMap<String, SoftwareTexture>,
    points: &[ui_core::geometry::Point],
    color: UiColor,
    thickness: i32,
    closed: bool,
    generated_index: &mut usize,
) {
    if points.len() < 2 {
        return;
    }
    for segment in points.windows(2) {
        if let Some((image_key, rect, texture)) =
            rasterize_line_command(segment[0], segment[1], color, thickness, *generated_index)
        {
            *generated_index += 1;
            textures.insert(image_key.clone(), texture);
            commands.push(FrameCommand::Image(ImageRequest {
                rect,
                clip_rect: None,
                image_key,
                alpha: 1.0,
            }));
        }
    }
    if closed {
        if let Some((image_key, rect, texture)) = rasterize_line_command(
            *points.last().unwrap_or(&points[0]),
            points[0],
            color,
            thickness,
            *generated_index,
        ) {
            *generated_index += 1;
            textures.insert(image_key.clone(), texture);
            commands.push(FrameCommand::Image(ImageRequest {
                rect,
                clip_rect: None,
                image_key,
                alpha: 1.0,
            }));
        }
    }
}

fn append_rasterized_particle_textures(
    commands: &mut Vec<FrameCommand>,
    textures: &mut HashMap<String, SoftwareTexture>,
    particles: &[ui_core::Particle],
    generated_index: &mut usize,
) {
    for particle in particles {
        if let Some((image_key, rect, texture)) = rasterize_circle_command(
            particle.center,
            particle.radius.max(1.0),
            particle.color,
            *generated_index,
        ) {
            *generated_index += 1;
            textures.insert(image_key.clone(), texture);
            commands.push(FrameCommand::Image(ImageRequest {
                rect,
                clip_rect: None,
                image_key,
                alpha: 1.0,
            }));
        }
    }
}

struct OwnedSoftwareSurface {
    width: usize,
    height: usize,
    stride: usize,
    bytes: Vec<u8>,
}

impl OwnedSoftwareSurface {
    fn new(width: usize, height: usize) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        Self {
            width,
            height,
            stride: width,
            bytes: vec![0; width * height * 4],
        }
    }

    fn is_blank(&self) -> bool {
        self.bytes.iter().all(|byte| *byte == 0)
    }

    fn into_texture(self) -> SoftwareTexture {
        SoftwareTexture {
            width: self.width,
            height: self.height,
            rgba8: Arc::from(self.bytes),
        }
    }

    fn draw_text(
        &mut self,
        request: &loadngo_renderer::TextRequest,
        current_font: Option<&SoftwareFont>,
    ) {
        let Some(font) = current_font else {
            return;
        };
        let layout = software_text_line_layout(Some(font), request.style.font_size, 1.0);
        let px = layout.px;
        let measured_line_height = layout.line_height.max(1) as f32;
        let line_box_height = single_line_text_box_height(request.style.font_size);
        let line_step = multiline_line_step(request.style.font_size);
        let baseline_offset =
            layout.baseline_offset as f32 + (line_box_height - measured_line_height).max(0.0) * 0.5;
        let normalized_text = match request.style.layout_mode {
            loadngo_host_core::RenderTextLayoutMode::SingleLine => request.text.replace('\n', " "),
            loadngo_host_core::RenderTextLayoutMode::MultiLine => request.text.clone(),
        };
        let lines: Vec<&str> = normalized_text.split('\n').collect();
        let mut total_height = match request.style.layout_mode {
            loadngo_host_core::RenderTextLayoutMode::SingleLine => line_box_height,
            loadngo_host_core::RenderTextLayoutMode::MultiLine => {
                line_box_height + line_step * lines.len().saturating_sub(1) as f32
            }
        };
        if total_height <= 0.0 {
            total_height = line_box_height;
        }

        let mut origin_y = 0.0;
        origin_y += match request.style.vertical_align {
            loadngo_host_core::RenderTextVerticalAlign::Top => 0.0,
            loadngo_host_core::RenderTextVerticalAlign::Middle => {
                (request.rect.height - total_height).max(0.0) * 0.5
            }
            loadngo_host_core::RenderTextVerticalAlign::Bottom => {
                (request.rect.height - total_height).max(0.0)
            }
        };

        for (line_index, line) in lines.iter().enumerate() {
            let line_metrics = font_text_metrics(
                line,
                Some(&DesktopFont {
                    source_path: None,
                    software_font: Some(font.clone()),
                }),
                request.style.font_size,
                1.0,
            );
            let mut cursor_x = 0.0;
            cursor_x += match request.style.horizontal_align {
                loadngo_host_core::RenderTextHorizontalAlign::Left => 0.0,
                loadngo_host_core::RenderTextHorizontalAlign::Center => {
                    (request.rect.width - line_metrics.width).max(0.0) * 0.5
                }
                loadngo_host_core::RenderTextHorizontalAlign::Right => {
                    (request.rect.width - line_metrics.width).max(0.0)
                }
            };
            let baseline_y = origin_y + line_index as f32 * line_step + baseline_offset;
            for ch in line.chars() {
                if ch == ' ' {
                    let metrics = font.inner.metrics(ch, px);
                    cursor_x += metrics.advance_width.max(px * 0.3);
                    continue;
                }
                let (metrics, bitmap) = font.inner.rasterize(ch, px);
                if metrics.width == 0 || metrics.height == 0 || bitmap.is_empty() {
                    cursor_x += metrics.advance_width;
                    continue;
                }
                let glyph_x = cursor_x + metrics.xmin as f32;
                let glyph_y = baseline_y - metrics.height as f32 - metrics.ymin as f32;
                for row in 0..metrics.height {
                    for col in 0..metrics.width {
                        let coverage = bitmap[row * metrics.width + col];
                        if coverage == 0 {
                            continue;
                        }
                        let color = UiColor::rgba(
                            request.style.color.r,
                            request.style.color.g,
                            request.style.color.b,
                            coverage,
                        );
                        self.write_pixel(
                            (glyph_x + col as f32).round() as i32,
                            (glyph_y + row as f32).round() as i32,
                            color,
                            1.0,
                        );
                    }
                }
                cursor_x += metrics.advance_width;
            }
        }
    }

    fn line(
        &mut self,
        from: ui_core::geometry::Point,
        to: ui_core::geometry::Point,
        color: UiColor,
        thickness: i32,
    ) {
        let thickness = thickness.max(1) as f32;
        let mut x0 = from.x.round();
        let mut y0 = from.y.round();
        let x1 = to.x.round();
        let y1 = to.y.round();
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1.0 } else { -1.0 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1.0 } else { -1.0 };
        let mut err = dx + dy;

        loop {
            let half = thickness * 0.5;
            self.fill_rect(
                UiRect {
                    x: x0 - half,
                    y: y0 - half,
                    width: thickness,
                    height: thickness,
                },
                color,
            );
            if x0 == x1 && y0 == y1 {
                break;
            }
            let e2 = err * 2.0;
            if e2 >= dy {
                err += dy;
                x0 += sx;
            }
            if e2 <= dx {
                err += dx;
                y0 += sy;
            }
        }
    }

    fn circle(&mut self, center_x: f32, center_y: f32, radius: f32, color: UiColor) {
        if radius <= 0.0 {
            return;
        }
        let radius_i = radius.ceil() as i32;
        let r2 = radius * radius;
        let cx = center_x.round() as i32;
        let cy = center_y.round() as i32;
        for y in -radius_i..=radius_i {
            for x in -radius_i..=radius_i {
                if (x * x + y * y) as f32 <= r2 {
                    self.write_pixel(cx + x, cy + y, color, 1.0);
                }
            }
        }
    }

    fn fill_rect(&mut self, rect: UiRect, color: UiColor) {
        let Some((x0, y0, x1, y1)) = self.clip_rect(rect) else {
            return;
        };
        for y in y0..y1 {
            for x in x0..x1 {
                self.write_pixel(x, y, color, 1.0);
            }
        }
    }

    fn clip_rect(&self, rect: UiRect) -> Option<(i32, i32, i32, i32)> {
        clip_rect_to_surface(rect, self.width, self.height)
    }

    fn write_pixel(&mut self, x: i32, y: i32, color: UiColor, extra_alpha: f32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let index = (y as usize * self.stride + x as usize) * 4;
        if index + 3 >= self.bytes.len() {
            return;
        }
        let alpha = ((color.a as f32 / 255.0) * extra_alpha.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        if alpha <= 0.0 {
            return;
        }
        if alpha >= 1.0 {
            self.bytes[index] = color.r;
            self.bytes[index + 1] = color.g;
            self.bytes[index + 2] = color.b;
            self.bytes[index + 3] = 255;
            return;
        }
        let inv = 1.0 - alpha;
        self.bytes[index] = (color.r as f32 * alpha + self.bytes[index] as f32 * inv).round() as u8;
        self.bytes[index + 1] =
            (color.g as f32 * alpha + self.bytes[index + 1] as f32 * inv).round() as u8;
        self.bytes[index + 2] =
            (color.b as f32 * alpha + self.bytes[index + 2] as f32 * inv).round() as u8;
        self.bytes[index + 3] = 255;
    }
}

fn clip_rect_to_surface(rect: UiRect, width: usize, height: usize) -> Option<(i32, i32, i32, i32)> {
    let x0 = rect.x.max(0.0).floor().min(width as f32) as i32;
    let y0 = rect.y.max(0.0).floor().min(height as f32) as i32;
    let x1 = rect.right().max(0.0).ceil().min(width as f32) as i32;
    let y1 = rect.bottom().max(0.0).ceil().min(height as f32) as i32;
    if x1 <= x0 || y1 <= y0 {
        None
    } else {
        Some((x0, y0, x1, y1))
    }
}

fn intersect_rects(a: UiRect, b: UiRect) -> Option<UiRect> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = a.right().min(b.right());
    let y1 = a.bottom().min(b.bottom());
    let width = x1 - x0;
    let height = y1 - y0;
    if width <= 0.0 || height <= 0.0 {
        None
    } else {
        Some(UiRect {
            x: x0,
            y: y0,
            width,
            height,
        })
    }
}

fn advance_frame_clock() {
    let mut state = app_state().lock().expect("android app state poisoned");
    let now = Instant::now();
    let dt = now.duration_since(state.last_tick).as_secs_f32();
    state.last_tick = now;
    state.timing = FrameTiming {
        delta_seconds: if dt > 0.0 { dt } else { 1.0 / 60.0 },
    };
    if let Some(window) = state.window.as_ref() {
        state.surface = logical_surface_info(window, state.display_scale);
    }
    state.frame_counter = state.frame_counter.saturating_add(1);
    wake_next_frame_waiters(&mut state);
}

fn poll_entry_future(mut future: Pin<&mut dyn Future<Output = ()>>) -> bool {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    matches!(future.as_mut().poll(&mut cx), Poll::Ready(()))
}

fn drain_input_queue(queue: *mut ndk_sys::AInputQueue) {
    if queue.is_null() {
        return;
    }

    unsafe {
        while ndk_sys::AInputQueue_hasEvents(queue) > 0 {
            let mut event: *mut ndk_sys::AInputEvent = std::ptr::null_mut();
            if ndk_sys::AInputQueue_getEvent(queue, &mut event) < 0 || event.is_null() {
                break;
            }
            if ndk_sys::AInputQueue_preDispatchEvent(queue, event) != 0 {
                continue;
            }
            let handled = handle_input_event(event);
            ndk_sys::AInputQueue_finishEvent(queue, event, if handled { 1 } else { 0 });
        }
    }
}

fn handle_input_event(event: *mut ndk_sys::AInputEvent) -> bool {
    unsafe {
        match ndk_sys::AInputEvent_getType(event.cast_const()) {
            AINPUT_EVENT_TYPE_MOTION_I32 => {
                handle_motion_event(event.cast_const());
                true
            }
            AINPUT_EVENT_TYPE_KEY_I32 => handle_key_event(event.cast_const()),
            _ => false,
        }
    }
}

unsafe fn handle_key_event(event: *const ndk_sys::AInputEvent) -> bool {
    let action = unsafe { ndk_sys::AKeyEvent_getAction(event) };
    let key_code = unsafe { ndk_sys::AKeyEvent_getKeyCode(event) };
    let is_down = action == AKEY_EVENT_ACTION_DOWN_I32;
    let is_up = action == AKEY_EVENT_ACTION_UP_I32;
    if !is_down && !is_up {
        return false;
    }

    let handled = {
        let mut state = app_state().lock().expect("android app state poisoned");
        let input = &mut state.input;
        let handled = match key_code {
            x if x == AKEYCODE_BACK_I32 || x == AKEYCODE_ESCAPE_I32 => {
                input.escape_pressed = is_down;
                true
            }
            x if x == AKEYCODE_SPACE_I32 || x == AKEYCODE_DPAD_CENTER_I32 => {
                input.space_pressed = is_down;
                input.space_down = is_down;
                true
            }
            x if x == AKEYCODE_F3_I32 => {
                input.f3_pressed = is_down;
                true
            }
            x if x == AKEYCODE_R_I32 => {
                input.r_pressed = is_down;
                true
            }
            x if x == AKEYCODE_DPAD_UP_I32 => {
                input.up_pressed = is_down;
                true
            }
            x if x == AKEYCODE_DPAD_DOWN_I32 => {
                input.down_pressed = is_down;
                true
            }
            _ => false,
        };
        if handled {
            state.event_epoch = state.event_epoch.saturating_add(1);
            wake_next_frame_waiters(&mut state);
        }
        handled
    };
    if handled {
        request_frame_callback();
    }
    handled
}

/// Applies a phase transition to a tracked touch, preserving an unobserved
/// `Started` against *any* later phase — not just a same-window terminal
/// phase, but a same-window `Moved` too, since a real finger's tap or
/// press-and-drag is rarely perfectly stationary. Once `Started` has
/// actually been observed by `capture_frame` (this point's phase is no
/// longer `Started` here), every later phase applies immediately as normal.
/// See `AndroidAppState::pending_touch_release`'s doc comment for why a
/// fast tap needs this to register at all, and
/// `loadngo/host-desktop/src/ios.rs`'s copy of this same function for the
/// fuller writeup of why guarding `Moved` too (not just terminal phases)
/// turned out to matter — found there first, ported back here for the same
/// underlying race, even though iOS's touch delivery makes it easier to
/// trigger in practice.
fn apply_touch_phase(
    point: &mut TouchPoint,
    new_phase: TouchPhase,
    pending_release: &mut Vec<u64>,
) {
    if point.phase == TouchPhase::Started {
        if matches!(new_phase, TouchPhase::Ended | TouchPhase::Cancelled)
            && !pending_release.contains(&point.id)
        {
            pending_release.push(point.id);
        }
        return;
    }
    point.phase = new_phase;
}

unsafe fn handle_motion_event(event: *const ndk_sys::AInputEvent) {
    let action = unsafe { ndk_sys::AMotionEvent_getAction(event) };
    let action_masked = action & AMOTION_EVENT_ACTION_MASK;
    // For a POINTER_DOWN/POINTER_UP event, `getPointerCount`/`getX`/`getY` report
    // every currently-active pointer, not just the one whose state actually
    // changed — the acting pointer is identified separately by this index. A
    // DOWN/UP (non-pointer) event always has exactly one pointer, so
    // `action_index` is trivially 0 there and this has no effect on the
    // single-touch case.
    let action_index = ((action & AMOTION_EVENT_ACTION_POINTER_INDEX_MASK_I32)
        >> AMOTION_EVENT_ACTION_POINTER_INDEX_SHIFT_I32) as usize;
    let pointer_count = unsafe { ndk_sys::AMotionEvent_getPointerCount(event) }.min(8);

    {
        let mut state = app_state().lock().expect("android app state poisoned");
        let display_scale = state.display_scale;
        // `MutexGuard`'s `DerefMut` hides disjoint-field-borrow splitting
        // from the borrow checker, so reborrow through a plain `&mut`
        // first — `input`/`pending_release` below then split cleanly.
        let state = &mut *state;
        let input = &mut state.input;
        let pending_release = &mut state.pending_touch_release;
        // `input.touches` is persistent multi-touch state, not a per-event
        // snapshot: it must NOT be wiped and rebuilt from this event's own
        // pointer list. Android only lists currently-active pointers in each
        // event, so a pointer that already went Ended (and hasn't been
        // consumed by `capture_frame`'s decay yet) is legitimately absent from
        // a later, unrelated event's pointer list. Wiping the array here used
        // to silently erase that still-pending `Ended` phase before the game
        // ever read it — the game would then keep tracking a touch id that no
        // longer exists on the digitizer at all, with no further phase
        // transition ever arriving to release it, which is exactly what
        // "the stick sticks at its last touch position" looks like from the
        // game side. Instead, look up each reported pointer by id and update
        // its existing slot in place (or claim a free slot for a genuinely
        // new pointer); slots this event doesn't mention are left untouched.
        for index in 0..pointer_count {
            let x = logical_point(
                unsafe { ndk_sys::AMotionEvent_getX(event, index) },
                display_scale,
            );
            let y = logical_point(
                unsafe { ndk_sys::AMotionEvent_getY(event, index) },
                display_scale,
            );
            let id = unsafe { ndk_sys::AMotionEvent_getPointerId(event, index) as u64 };
            // MOVE and CANCEL apply uniformly to every currently-listed pointer.
            // DOWN/POINTER_DOWN/UP/POINTER_UP only describe a phase transition
            // for the specific pointer at `action_index`; every other pointer
            // reported alongside it in this same event is just continuing to
            // be held, unchanged, so it must not be stamped Started/Ended too
            // (that previously ended or re-began every other active touch —
            // including live joystick drags — whenever any other finger, such
            // as a tap on a separate on-screen button, went down or up).
            let phase = if action_masked == AMOTION_EVENT_ACTION_CANCEL_I32 {
                TouchPhase::Cancelled
            } else if action_masked == AMOTION_EVENT_ACTION_MOVE_I32 {
                TouchPhase::Moved
            } else if index == action_index {
                match action_masked {
                    x if x == AMOTION_EVENT_ACTION_DOWN_I32
                        || x == AMOTION_EVENT_ACTION_POINTER_DOWN_I32 =>
                    {
                        TouchPhase::Started
                    }
                    x if x == AMOTION_EVENT_ACTION_UP_I32
                        || x == AMOTION_EVENT_ACTION_POINTER_UP_I32 =>
                    {
                        TouchPhase::Ended
                    }
                    _ => TouchPhase::Stationary,
                }
            } else {
                TouchPhase::Stationary
            };
            if let Some(existing) = input.touches.iter_mut().flatten().find(|p| p.id == id) {
                existing.x = x;
                existing.y = y;
                apply_touch_phase(existing, phase, pending_release);
            } else if let Some(free_slot) = input.touches.iter_mut().find(|slot| slot.is_none()) {
                *free_slot = Some(TouchPoint { id, x, y, phase });
            }
            if index == 0 {
                input.mouse_x = x;
                input.mouse_y = y;
            }
        }
        if action_masked == AMOTION_EVENT_ACTION_CANCEL_I32 {
            // A cancel applies to the whole gesture: every pointer this
            // AInputEvent's own pointer list didn't already cover (e.g. one
            // already mid-decay to Stationary from an earlier event) must
            // still be released, or it would be the exact same kind of
            // permanently stuck touch this whole rewrite exists to prevent.
            for slot in input.touches.iter_mut().flatten() {
                apply_touch_phase(slot, TouchPhase::Cancelled, pending_release);
            }
        }

        let pointer_active = !matches!(
            action_masked,
            x if x == AMOTION_EVENT_ACTION_UP_I32 || x == AMOTION_EVENT_ACTION_CANCEL_I32
        );
        input.mouse_pressed = matches!(
            action_masked,
            x if x == AMOTION_EVENT_ACTION_DOWN_I32
                || x == AMOTION_EVENT_ACTION_POINTER_DOWN_I32
        );
        input.mouse_released = matches!(
            action_masked,
            x if x == AMOTION_EVENT_ACTION_UP_I32
                || x == AMOTION_EVENT_ACTION_POINTER_UP_I32
                || x == AMOTION_EVENT_ACTION_CANCEL_I32
        );
        input.mouse_down = pointer_active;
        state.event_epoch = state.event_epoch.saturating_add(1);
        wake_next_frame_waiters(state);
    }
    request_frame_callback();
}

struct NextFrameFuture {
    demand: FrameDemand,
    observed_event_epoch: u64,
    target_frame: u64,
    flushed: bool,
    scheduled: bool,
}

impl NextFrameFuture {
    fn new(demand: FrameDemand) -> Self {
        let (observed_event_epoch, target_frame) = {
            let state = app_state().lock().expect("android app state poisoned");
            (state.event_epoch, state.frame_counter.saturating_add(1))
        };
        Self {
            demand,
            observed_event_epoch,
            target_frame,
            flushed: false,
            scheduled: false,
        }
    }
}

impl Future for NextFrameFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if !self.flushed {
            flush_queued_frame();
            self.flushed = true;
        }
        let mut state = app_state().lock().expect("android app state poisoned");
        let ready = match self.demand {
            FrameDemand::Idle => state.event_epoch > self.observed_event_epoch,
            FrameDemand::After(_) => state.frame_counter >= self.target_frame,
        };
        if ready {
            Poll::Ready(())
        } else {
            state.next_frame_wakers.push(cx.waker().clone());
            drop(state);
            if !self.scheduled {
                if let FrameDemand::After(_) = self.demand {
                    request_frame_callback();
                    self.scheduled = true;
                }
            }
            Poll::Pending
        }
    }
}

pub fn capture_frame() -> HostFrame {
    let mut state = app_state().lock().expect("android app state poisoned");
    let frame = HostFrame {
        timing: state.timing,
        surface: state.surface,
        input: state.input.clone(),
        foreground: state.foreground,
        insets: state.insets,
    };
    let state_mut = &mut *state;
    let input = &mut state_mut.input;
    let pending_release = &mut state_mut.pending_touch_release;
    for touch in &mut input.touches {
        match touch {
            Some(point) => match point.phase {
                TouchPhase::Started | TouchPhase::Moved => {
                    point.phase = TouchPhase::Stationary;
                    // This touch's `Started` (or `Moved`) has now been
                    // observed in the `frame` snapshot above — if a
                    // deferred fast-tap release was waiting on exactly
                    // that observation (see `pending_touch_release`'s doc
                    // comment), it can be applied immediately.
                    if let Some(index) = pending_release.iter().position(|&id| id == point.id) {
                        pending_release.swap_remove(index);
                        *touch = None;
                    }
                }
                TouchPhase::Ended | TouchPhase::Cancelled => {
                    *touch = None;
                }
                TouchPhase::Stationary => {}
            },
            None => {}
        }
    }
    state.input.mouse_pressed = false;
    state.input.mouse_released = false;
    state.input.escape_pressed = false;
    state.input.space_pressed = false;
    state.input.f3_pressed = false;
    state.input.r_pressed = false;
    state.input.up_pressed = false;
    state.input.down_pressed = false;
    frame
}

/// A writable, per-app directory for small local persistence (achievement
/// records, local profile state, ...) — created if it doesn't exist yet.
/// `Context.getFilesDir()` already returns an app-private, sandboxed
/// directory (`/data/data/<package>/files`), so `app_id` isn't needed to
/// avoid collisions the way the desktop backends need it — it's still
/// accepted for signature parity across platforms.
pub fn app_data_dir(_app_id: &str) -> Result<String, String> {
    android_jni::with_env(|env| {
        let ctx = ndk_context::android_context();
        let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
        let files_dir =
            android_jni::call_object(env, &activity, "getFilesDir", "()Ljava/io/File;", &[])?
                .ok_or_else(|| "Context.getFilesDir() returned null".to_string())?;
        let path_obj = android_jni::call_object(
            env,
            &files_dir,
            "getAbsolutePath",
            "()Ljava/lang/String;",
            &[],
        )?
        .ok_or_else(|| "File.getAbsolutePath() returned null".to_string())?;
        let path_string = jni::objects::JString::from(path_obj);
        env.get_string(&path_string)
            .map(|value| value.to_string_lossy().into_owned())
            .map_err(|err| format!("failed to decode files dir path: {err}"))
    })
}

pub async fn load_bytes(path: &str) -> Result<Vec<u8>, String> {
    if let Ok(bytes) = std::fs::read(path) {
        return Ok(bytes);
    }
    let manager = current_asset_manager()?;
    let asset_path = resolve_asset_rel_for_path(path);
    let asset_name = CString::new(asset_path.as_str())
        .map_err(|_| format!("invalid Android asset path: {asset_path:?}"))?;
    let Some(mut asset) = manager.open(asset_name.as_c_str()) else {
        return Err(format!("Android asset not found: {asset_path}"));
    };
    match asset.buffer() {
        Ok(buffer) => Ok(buffer.to_vec()),
        Err(_) => {
            let mut bytes = Vec::new();
            asset
                .read_to_end(&mut bytes)
                .map_err(|err| format!("failed to read Android asset {asset_path}: {err}"))?;
            Ok(bytes)
        }
    }
}

pub async fn load_text(path: &str) -> Result<String, String> {
    let bytes = load_bytes(path).await?;
    String::from_utf8(bytes).map_err(|err| format!("Android asset {path} is not UTF-8: {err}"))
}

pub async fn load_font(path: &str) -> Result<DesktopFont, String> {
    let mut attempted = vec![path.to_string()];
    let (source_path, software_font) = match load_software_font_from_path(path).await {
        Ok(font) => (path.to_string(), font),
        Err(primary_err) => {
            android_log_error(&primary_err);
            let mut fallback_font = None;
            let mut last_err = primary_err;
            for candidate in android_platform_font_candidates() {
                if attempted.iter().any(|attempt| attempt == candidate) {
                    continue;
                }
                attempted.push((*candidate).to_string());
                match load_software_font_from_path(candidate).await {
                    Ok(font) => {
                        android_log_info(&format!("Android font fallback selected: {candidate}"));
                        fallback_font = Some((candidate.to_string(), font));
                        break;
                    }
                    Err(err) => {
                        android_log_error(&err);
                        last_err = err;
                    }
                }
            }
            fallback_font.ok_or(last_err)?
        }
    };
    {
        let mut state = app_state().lock().expect("android app state poisoned");
        state.current_font = Some(software_font.clone());
        state.default_font = Some(software_font.clone());
    }
    Ok(DesktopFont::new(Some(source_path), Some(software_font)))
}

/// See `base_language_tag`'s doc comment in `lib.rs` for the contract.
/// `Locale.getLanguage()` already returns a bare ISO 639 language code
/// with no region/script subtag (unlike `NSLocale`'s BCP-47 tags or a
/// POSIX `LANG` string), so `base_language_tag` mostly just normalizes
/// case and handles the null/error case identically to every other
/// platform here.
#[must_use]
pub fn system_locale() -> String {
    android_jni::with_env(|env| {
        let locale = android_jni::call_static_object(
            env,
            "java/util/Locale",
            "getDefault",
            "()Ljava/util/Locale;",
            &[],
        )?
        .ok_or_else(|| "Locale.getDefault() returned null".to_string())?;
        let language =
            android_jni::call_object(env, &locale, "getLanguage", "()Ljava/lang/String;", &[])?
                .ok_or_else(|| "Locale.getLanguage() returned null".to_string())?;
        android_jni::java_string_to_rust(env, language)
    })
    .ok()
    .and_then(|raw| crate::base_language_tag(&raw))
    .unwrap_or_else(|| "en".to_string())
}

pub fn measure_text_metrics(
    text: &str,
    font: Option<&DesktopFont>,
    font_size: u16,
    font_scale: f32,
) -> TextMetrics {
    font_text_metrics(text, font, font_size, font_scale)
}

pub fn wrap_text_lines(
    text: &str,
    font: Option<&DesktopFont>,
    font_size: u16,
    font_scale: f32,
    max_width: f32,
) -> Vec<String> {
    let mut lines = Vec::new();
    for raw_line in text.split('\n') {
        if raw_line.trim().is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current_line = String::new();
        for word in raw_line.split_whitespace() {
            if current_line.is_empty() {
                current_line.push_str(word);
                continue;
            }
            let candidate = format!("{} {}", current_line, word);
            if measure_text_metrics(&candidate, font, font_size, font_scale).width <= max_width {
                current_line = candidate;
            } else {
                lines.push(current_line);
                current_line = word.to_string();
            }
        }
        if !current_line.is_empty() {
            lines.push(current_line);
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn software_present(
    window: &NativeWindow,
    commands: &[FrameCommand],
    textures: &HashMap<String, SoftwareTexture>,
    current_font: Option<&SoftwareFont>,
) -> Result<(), String> {
    let width = window.width();
    let height = window.height();
    if width <= 0 || height <= 0 {
        return Ok(());
    }
    window
        .set_buffers_geometry(width, height, Some(HardwareBufferFormat::R8G8B8A8_UNORM))
        .map_err(|err| format!("failed to configure Android window buffer geometry: {err}"))?;

    let mut buffer = window
        .lock(None)
        .map_err(|err| format!("failed to lock Android native window: {err}"))?;
    let Some(raw) = buffer.bytes() else {
        return Err("Android native window buffer format is unsupported".to_string());
    };
    let bytes = unsafe { std::slice::from_raw_parts_mut(raw.as_mut_ptr().cast::<u8>(), raw.len()) };
    let mut framebuffer = SoftwareFramebuffer {
        width: buffer.width(),
        height: buffer.height(),
        stride: buffer.stride(),
        bytes,
    };
    framebuffer.ensure_initialized();
    for command in commands {
        framebuffer.execute(command, textures, current_font);
    }
    Ok(())
}

struct SoftwareFramebuffer<'a> {
    width: usize,
    height: usize,
    stride: usize,
    bytes: &'a mut [u8],
}

impl<'a> SoftwareFramebuffer<'a> {
    fn ensure_initialized(&mut self) {
        for byte in self.bytes.iter_mut() {
            if *byte != 0 {
                return;
            }
        }
        self.clear(UiColor::rgba(0, 0, 0, 255));
    }

    fn execute(
        &mut self,
        command: &FrameCommand,
        textures: &HashMap<String, SoftwareTexture>,
        current_font: Option<&SoftwareFont>,
    ) {
        match command {
            FrameCommand::Clear { color } => self.clear(*color),
            FrameCommand::FillRect { rect, color } => self.fill_rect(*rect, *color),
            FrameCommand::StrokeRect {
                rect,
                color,
                thickness,
            } => self.stroke_rect(*rect, *color, *thickness),
            FrameCommand::Line {
                from,
                to,
                color,
                thickness,
            } => self.line(*from, *to, *color, *thickness),
            FrameCommand::Circle {
                center,
                radius,
                color,
            } => self.circle(center.x, center.y, *radius, *color),
            FrameCommand::Arc {
                center,
                radius,
                start_angle,
                sweep_angle,
                color,
                thickness,
            } => {
                let points = approximate_arc_points(*center, *radius, *start_angle, *sweep_angle);
                self.polyline(points.as_slice(), *color, *thickness, false);
            }
            FrameCommand::Polyline {
                points,
                color,
                thickness,
                closed,
            } => self.polyline(points.as_slice(), *color, *thickness, *closed),
            FrameCommand::ParticleBatch { particles } => {
                for particle in particles {
                    self.circle(
                        particle.center.x,
                        particle.center.y,
                        particle.radius.max(1.0),
                        particle.color,
                    );
                }
            }
            FrameCommand::Image(request) => self.blit_image(request, textures),
            FrameCommand::Text(request) => self.draw_text(request, current_font),
        }
    }

    fn clear(&mut self, color: UiColor) {
        for y in 0..self.height as i32 {
            for x in 0..self.width as i32 {
                self.write_pixel(x, y, color, 1.0);
            }
        }
    }

    fn fill_rect(&mut self, rect: UiRect, color: UiColor) {
        let Some((x0, y0, x1, y1)) = self.clip_rect(rect) else {
            return;
        };
        for y in y0..y1 {
            for x in x0..x1 {
                self.write_pixel(x, y, color, 1.0);
            }
        }
    }

    fn stroke_rect(&mut self, rect: UiRect, color: UiColor, thickness: i32) {
        let thickness = thickness.max(1) as f32;
        self.fill_rect(
            UiRect {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: thickness,
            },
            color,
        );
        self.fill_rect(
            UiRect {
                x: rect.x,
                y: rect.bottom() - thickness,
                width: rect.width,
                height: thickness,
            },
            color,
        );
        self.fill_rect(
            UiRect {
                x: rect.x,
                y: rect.y,
                width: thickness,
                height: rect.height,
            },
            color,
        );
        self.fill_rect(
            UiRect {
                x: rect.right() - thickness,
                y: rect.y,
                width: thickness,
                height: rect.height,
            },
            color,
        );
    }

    fn polyline(
        &mut self,
        points: &[ui_core::geometry::Point],
        color: UiColor,
        thickness: i32,
        closed: bool,
    ) {
        if points.len() < 2 {
            return;
        }
        for segment in points.windows(2) {
            self.line(segment[0], segment[1], color, thickness);
        }
        if closed {
            self.line(
                *points.last().unwrap_or(&points[0]),
                points[0],
                color,
                thickness,
            );
        }
    }

    fn line(
        &mut self,
        from: ui_core::geometry::Point,
        to: ui_core::geometry::Point,
        color: UiColor,
        thickness: i32,
    ) {
        let thickness = thickness.max(1) as f32;
        let mut x0 = from.x.round();
        let mut y0 = from.y.round();
        let x1 = to.x.round();
        let y1 = to.y.round();
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1.0 } else { -1.0 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1.0 } else { -1.0 };
        let mut err = dx + dy;

        loop {
            let half = thickness * 0.5;
            self.fill_rect(
                UiRect {
                    x: x0 - half,
                    y: y0 - half,
                    width: thickness,
                    height: thickness,
                },
                color,
            );
            if x0 == x1 && y0 == y1 {
                break;
            }
            let e2 = err * 2.0;
            if e2 >= dy {
                err += dy;
                x0 += sx;
            }
            if e2 <= dx {
                err += dx;
                y0 += sy;
            }
        }
    }

    fn circle(&mut self, center_x: f32, center_y: f32, radius: f32, color: UiColor) {
        if radius <= 0.0 {
            return;
        }
        let radius = radius.max(1.0);
        let radius_i = radius.round() as i32;
        let r2 = radius * radius;
        let cx = center_x.round() as i32;
        let cy = center_y.round() as i32;
        for y in -radius_i..=radius_i {
            for x in -radius_i..=radius_i {
                if (x * x + y * y) as f32 <= r2 {
                    self.write_pixel(cx + x, cy + y, color, 1.0);
                }
            }
        }
    }

    fn blit_image(&mut self, request: &ImageRequest, textures: &HashMap<String, SoftwareTexture>) {
        let Some(texture) = textures.get(request.image_key.as_str()) else {
            return;
        };
        self.blit_software_texture(texture, request.rect, request.clip_rect, request.alpha);
    }

    fn draw_text(
        &mut self,
        request: &loadngo_renderer::TextRequest,
        current_font: Option<&SoftwareFont>,
    ) {
        let Some(font) = current_font else {
            return;
        };
        let layout = software_text_line_layout(Some(font), request.style.font_size, 1.0);
        let px = layout.px;
        let measured_line_height = layout.line_height.max(1) as f32;
        let line_box_height = single_line_text_box_height(request.style.font_size);
        let line_step = multiline_line_step(request.style.font_size);
        let baseline_offset =
            layout.baseline_offset as f32 + (line_box_height - measured_line_height).max(0.0) * 0.5;
        let normalized_text = match request.style.layout_mode {
            loadngo_host_core::RenderTextLayoutMode::SingleLine => request.text.replace('\n', " "),
            loadngo_host_core::RenderTextLayoutMode::MultiLine => request.text.clone(),
        };
        let lines: Vec<&str> = normalized_text.split('\n').collect();
        let mut total_height = match request.style.layout_mode {
            loadngo_host_core::RenderTextLayoutMode::SingleLine => line_box_height,
            loadngo_host_core::RenderTextLayoutMode::MultiLine => {
                line_box_height + line_step * lines.len().saturating_sub(1) as f32
            }
        };
        if total_height <= 0.0 {
            total_height = line_box_height;
        }

        let mut origin_y = request.rect.y;
        origin_y += match request.style.vertical_align {
            loadngo_host_core::RenderTextVerticalAlign::Top => 0.0,
            loadngo_host_core::RenderTextVerticalAlign::Middle => {
                (request.rect.height - total_height).max(0.0) * 0.5
            }
            loadngo_host_core::RenderTextVerticalAlign::Bottom => {
                (request.rect.height - total_height).max(0.0)
            }
        };

        for (line_index, line) in lines.iter().enumerate() {
            let line_metrics = font_text_metrics(
                line,
                Some(&DesktopFont {
                    source_path: None,
                    software_font: Some(font.clone()),
                }),
                request.style.font_size,
                1.0,
            );
            let mut cursor_x = request.rect.x;
            cursor_x += match request.style.horizontal_align {
                loadngo_host_core::RenderTextHorizontalAlign::Left => 0.0,
                loadngo_host_core::RenderTextHorizontalAlign::Center => {
                    (request.rect.width - line_metrics.width).max(0.0) * 0.5
                }
                loadngo_host_core::RenderTextHorizontalAlign::Right => {
                    (request.rect.width - line_metrics.width).max(0.0)
                }
            };
            let baseline_y = origin_y + line_index as f32 * line_step + baseline_offset;
            for ch in line.chars() {
                if ch == ' ' {
                    let metrics = font.inner.metrics(ch, px);
                    cursor_x += metrics.advance_width.max(px * 0.3);
                    continue;
                }
                let (metrics, bitmap) = font.inner.rasterize(ch, px);
                if metrics.width == 0 || metrics.height == 0 || bitmap.is_empty() {
                    cursor_x += metrics.advance_width;
                    continue;
                }
                let glyph_x = cursor_x + metrics.xmin as f32;
                let glyph_y = baseline_y - metrics.height as f32 - metrics.ymin as f32;
                for row in 0..metrics.height {
                    for col in 0..metrics.width {
                        let coverage = bitmap[row * metrics.width + col];
                        if coverage == 0 {
                            continue;
                        }
                        let color = UiColor::rgba(
                            request.style.color.r,
                            request.style.color.g,
                            request.style.color.b,
                            coverage,
                        );
                        self.write_pixel(
                            (glyph_x + col as f32).round() as i32,
                            (glyph_y + row as f32).round() as i32,
                            color,
                            1.0,
                        );
                    }
                }
                cursor_x += metrics.advance_width;
            }
        }
    }

    fn blit_software_texture(
        &mut self,
        texture: &SoftwareTexture,
        rect: UiRect,
        clip_rect: Option<UiRect>,
        alpha: f32,
    ) {
        if texture.is_empty() || rect.width <= 0.0 || rect.height <= 0.0 {
            return;
        }
        let draw_rect = if let Some(clip_rect) = clip_rect {
            let Some(draw_rect) = intersect_rects(rect, clip_rect) else {
                return;
            };
            draw_rect
        } else {
            rect
        };
        let Some((x0, y0, x1, y1)) = self.clip_rect(draw_rect) else {
            return;
        };

        for y in y0..y1 {
            let src_y = (((y as f32 - rect.y) / rect.height) * texture.height as f32)
                .floor()
                .clamp(0.0, (texture.height - 1) as f32) as usize;
            for x in x0..x1 {
                let src_x = (((x as f32 - rect.x) / rect.width) * texture.width as f32)
                    .floor()
                    .clamp(0.0, (texture.width - 1) as f32) as usize;
                let src_index = (src_y * texture.width + src_x) * 4;
                let color = UiColor::rgba(
                    texture.rgba8[src_index],
                    texture.rgba8[src_index + 1],
                    texture.rgba8[src_index + 2],
                    texture.rgba8[src_index + 3],
                );
                self.write_pixel(x, y, color, alpha);
            }
        }
    }

    fn clip_rect(&self, rect: UiRect) -> Option<(i32, i32, i32, i32)> {
        clip_rect_to_surface(rect, self.width, self.height)
    }

    fn write_pixel(&mut self, x: i32, y: i32, color: UiColor, extra_alpha: f32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let index = (y as usize * self.stride + x as usize) * 4;
        if index + 3 >= self.bytes.len() {
            return;
        }
        let alpha = ((color.a as f32 / 255.0) * extra_alpha.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        if alpha <= 0.0 {
            return;
        }
        if alpha >= 1.0 {
            self.bytes[index] = color.r;
            self.bytes[index + 1] = color.g;
            self.bytes[index + 2] = color.b;
            self.bytes[index + 3] = 255;
            return;
        }
        let inv = 1.0 - alpha;
        self.bytes[index] = (color.r as f32 * alpha + self.bytes[index] as f32 * inv).round() as u8;
        self.bytes[index + 1] =
            (color.g as f32 * alpha + self.bytes[index + 1] as f32 * inv).round() as u8;
        self.bytes[index + 2] =
            (color.b as f32 * alpha + self.bytes[index + 2] as f32 * inv).round() as u8;
        self.bytes[index + 3] = 255;
    }
}

fn approximate_arc_points(
    center: ui_core::geometry::Point,
    radius: f32,
    start_angle: f32,
    sweep_angle: f32,
) -> Vec<ui_core::geometry::Point> {
    if radius <= 0.0 || sweep_angle.abs() <= f32::EPSILON {
        return Vec::new();
    }
    let segment_count = ((radius.abs() * sweep_angle.abs()) / 10.0)
        .ceil()
        .clamp(8.0, 96.0) as usize;
    (0..=segment_count)
        .map(|index| {
            let t = index as f32 / segment_count as f32;
            let angle = start_angle + sweep_angle * t;
            ui_core::geometry::Point {
                x: center.x + radius * angle.cos(),
                y: center.y + radius * angle.sin(),
            }
        })
        .collect()
}

pub fn render_text_lines(
    lines: &[String],
    x: f32,
    y: f32,
    font: Option<&DesktopFont>,
    font_size: u16,
    font_scale: f32,
    color: UiColor,
    line_spacing: f32,
) {
    let mut ops = Vec::new();
    let line_height = multiline_line_step(font_size) * font_scale.max(0.0);
    let line_box_height = single_line_text_box_height(font_size) * font_scale.max(0.0);
    let mut current_y = y;
    for line in lines {
        if !line.is_empty() {
            let metrics = measure_text_metrics(line, font, font_size, font_scale);
            ops.push(RenderOp::Text {
                rect: UiRect {
                    x,
                    y: current_y,
                    width: metrics.width.max(1.0),
                    height: line_box_height,
                },
                text: line.clone(),
                style: loadngo_host_core::RenderTextStyle {
                    color,
                    font_size,
                    horizontal_align: loadngo_host_core::RenderTextHorizontalAlign::Left,
                    vertical_align: loadngo_host_core::RenderTextVerticalAlign::Top,
                    vertical_metric_mode:
                        loadngo_host_core::RenderTextVerticalMetricMode::LogicalLineBox,
                    layout_mode: loadngo_host_core::RenderTextLayoutMode::SingleLine,
                    overflow: loadngo_host_core::RenderTextOverflow::Clip,
                },
            });
        }
        current_y += line_height + line_spacing;
    }
    render_ops(&ops, font);
}

pub fn render_ops(ops: &[RenderOp], font: Option<&DesktopFont>) {
    if let Some(font) = font.and_then(|font| font.software_font.clone()) {
        let mut state = app_state().lock().expect("android app state poisoned");
        state.current_font = Some(font);
    } else {
        let mut state = app_state().lock().expect("android app state poisoned");
        ensure_default_font_loaded(&mut state);
        if state.current_font.is_none() {
            state.current_font = state.default_font.clone();
        }
    }
    queue_commands(renderer().encode_render_ops(ops));
}

pub fn render_widget_paint_ops(ops: &[PaintOp]) {
    let mut state = app_state().lock().expect("android app state poisoned");
    ensure_default_font_loaded(&mut state);
    if state.current_font.is_none() {
        state.current_font = state.default_font.clone();
    }
    drop(state);
    queue_commands(renderer().encode_paint_ops(ops));
}

pub fn clear(color: UiColor) {
    queue_commands([FrameCommand::Clear { color }]);
}

pub fn draw_plain_text(text: &str, _x: f32, _y: f32, size: f32, _color: UiColor) -> TextMetrics {
    let (font_size, font_scale) = font_size_and_scale(size);
    let metrics = measure_text_metrics(text, None, font_size, font_scale);
    queue_commands([FrameCommand::Text(loadngo_renderer::TextRequest {
        rect: UiRect {
            x: _x,
            y: _y,
            width: metrics.width.max(1.0),
            height: single_line_text_box_height(font_size) * font_scale.max(0.0),
        },
        clip_rect: None,
        text: text.to_string(),
        style: loadngo_host_core::RenderTextStyle {
            color: _color,
            font_size,
            horizontal_align: loadngo_host_core::RenderTextHorizontalAlign::Left,
            vertical_align: loadngo_host_core::RenderTextVerticalAlign::Top,
            vertical_metric_mode: loadngo_host_core::RenderTextVerticalMetricMode::LogicalLineBox,
            layout_mode: loadngo_host_core::RenderTextLayoutMode::SingleLine,
            overflow: loadngo_host_core::RenderTextOverflow::Clip,
        },
        font_source: None,
        direction: loadngo_renderer::TextDirection::Auto,
        script: loadngo_renderer::TextScript::Auto,
        language: None,
    })]);
    metrics
}

pub fn blit_texture(texture: &DesktopTexture, rect: UiRect, alpha: f32) {
    if let Some(image_key) = texture.image_key() {
        queue_commands([FrameCommand::Image(ImageRequest {
            rect,
            clip_rect: None,
            image_key: image_key.to_string(),
            alpha,
        })]);
        return;
    }

    let mut state = app_state().lock().expect("android app state poisoned");
    let image_key = format!(
        "inline://{}x{}:{}",
        texture.width as i32,
        texture.height as i32,
        state.texture_registry.len()
    );
    state
        .texture_registry
        .insert(image_key.clone(), texture.software_texture.clone());
    state
        .queued_commands
        .push(FrameCommand::Image(ImageRequest {
            rect,
            clip_rect: None,
            image_key,
            alpha,
        }));
}

pub fn upload_texture(image: &DecodedImage) -> Result<DesktopTexture, String> {
    image.validate_rgba8()?;
    Ok(DesktopTexture::new(
        None,
        SoftwareTexture::from_decoded_image(image),
    ))
}

pub fn upload_texture_with_image_key(
    image_key: Option<&str>,
    image: &DecodedImage,
) -> Result<DesktopTexture, String> {
    image.validate_rgba8()?;
    let software_texture = SoftwareTexture::from_decoded_image(image);
    if let Some(image_key) = image_key {
        let mut state = app_state().lock().expect("android app state poisoned");
        state
            .texture_registry
            .insert(image_key.to_string(), software_texture.clone());
    }
    Ok(DesktopTexture::new(
        image_key.map(str::to_string),
        software_texture,
    ))
}

pub fn draw_texture_fit(texture: &DesktopTexture, x: f32, y: f32, width: f32, height: f32) {
    blit_texture(
        texture,
        UiRect {
            x: x.round(),
            y: y.round(),
            width: width.round(),
            height: height.round(),
        },
        1.0,
    );
}

pub fn draw_rectangle(x: f32, y: f32, w: f32, h: f32, color: UiColor) {
    queue_commands([FrameCommand::FillRect {
        rect: UiRect {
            x: x.round(),
            y: y.round(),
            width: w.round().max(0.0),
            height: h.round().max(0.0),
        },
        color,
    }]);
}

pub fn draw_rectangle_lines(x: f32, y: f32, w: f32, h: f32, thickness: f32, color: UiColor) {
    queue_commands([FrameCommand::StrokeRect {
        rect: UiRect {
            x: x.round(),
            y: y.round(),
            width: w.round().max(0.0),
            height: h.round().max(0.0),
        },
        color,
        thickness: thickness.round().max(1.0) as i32,
    }]);
}

pub fn draw_text(text: &str, x: f32, y: f32, size: f32, color: UiColor) {
    let _ = draw_plain_text(text, x, y, size, color);
}

pub fn measure_text(text: &str, _font: Option<()>, font_size: u16, font_scale: f32) -> TextMetrics {
    approximate_text_metrics(text, font_size, font_scale)
}

pub async fn next_frame(demand: FrameDemand) {
    NextFrameFuture::new(demand).await;
}

pub fn simulate_mouse_with_touch(enabled: bool) {
    let mut state = app_state().lock().expect("android app state poisoned");
    state.simulate_mouse_with_touch = enabled;
}

fn noop_waker() -> Waker {
    unsafe fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(std::ptr::null(), &VTABLE)
    }
    unsafe fn wake(_: *const ()) {}
    unsafe fn wake_by_ref(_: *const ()) {}
    unsafe fn drop(_: *const ()) {}
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);
    let raw = RawWaker::new(std::ptr::null(), &VTABLE);
    unsafe { Waker::from_raw(raw) }
}
