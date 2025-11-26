use std::env;
use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Condvar, Mutex, OnceLock,
};
use std::thread;
use std::time::Instant;

use futures::executor::LocalPool;
use futures::task::LocalSpawnExt;
use loadngo_gfx_metal::{
    measure_text_metrics as metal_measure_text_metrics, register_image_resource, MetalBackend,
};
use loadngo_host_core::{
    DecodedImage, FrameDemand, FrameTiming, HostFrame, HostKey, HostKeyEvent, InputSnapshot,
    RenderOp, RenderTextStyle, SurfaceInfo, TextMetrics, TouchPhase, TouchPoint, WindowDescriptor,
    WindowIconSet,
};
use loadngo_renderer::{FrameCommand, ImageRequest, Renderer, RendererConfig};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send_id, sel, MainThreadOnly};
use objc2_foundation::{MainThreadMarker, NSObject};
use objc2_ui_kit::{UIGestureRecognizerState, UILongPressGestureRecognizer, UIView};
use ui_core::{
    geometry::{Color as UiColor, Rect as UiRect},
    paint::PaintOp,
    Modifiers,
};
use winit::application::ApplicationHandler;
use winit::event::{
    ElementState, Ime, Touch as WinitTouch, TouchPhase as WinitTouchPhase, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WinitKey, KeyCode, NamedKey, PhysicalKey};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowAttributes, WindowId};

#[derive(Clone)]
pub struct DesktopFont {
    source_path: Option<String>,
}

impl DesktopFont {
    fn new(source_path: Option<String>) -> Self {
        Self { source_path }
    }

    pub fn source_path(&self) -> Option<&str> {
        self.source_path.as_deref()
    }
}

#[derive(Clone)]
pub struct DesktopTexture {
    image_key: String,
    width: f32,
    height: f32,
}

impl DesktopTexture {
    fn new(image_key: String, width: f32, height: f32) -> Self {
        Self {
            image_key,
            width,
            height,
        }
    }

    pub fn width(&self) -> f32 {
        self.width
    }

    pub fn height(&self) -> f32 {
        self.height
    }

    pub fn image_key(&self) -> &str {
        &self.image_key
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopRenderBackendKind {
    Metal,
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

#[derive(Clone)]
struct IosHostShared {
    state: Arc<(Mutex<HostSharedState>, Condvar)>,
}

#[derive(Debug, Clone, Copy)]
enum IosUserEvent {
    Wake,
}

struct HostSharedState {
    latest_frame: HostFrame,
    pending_input: PendingInput,
    safe_area_insets: (f32, f32, f32, f32),
    frame_epoch: u64,
    last_frame_instant: Instant,
    running: bool,
    simulate_mouse_with_touch: bool,
    pending_redraw: bool,
    queued_commands: Vec<FrameCommand>,
    queued_font_source: Option<String>,
    last_submitted_commands: Vec<FrameCommand>,
    last_submitted_font_source: Option<String>,
    next_texture_id: u64,
    last_backend_used: DesktopRenderBackendKind,
    backend_detail: String,
    event_proxy: Option<EventLoopProxy<IosUserEvent>>,
}

#[derive(Clone)]
struct PendingInput {
    mouse_x: f32,
    mouse_y: f32,
    mouse_wheel_x: f32,
    mouse_wheel_y: f32,
    mouse_pressed: bool,
    mouse_down: bool,
    mouse_released: bool,
    touches: [Option<TouchPoint>; 8],
    escape_pressed: bool,
    space_pressed: bool,
    space_down: bool,
    f3_pressed: bool,
    r_pressed: bool,
    up_pressed: bool,
    down_pressed: bool,
    modifiers: Modifiers,
    key_events: Vec<HostKeyEvent>,
    typed_text: String,
}

impl Default for PendingInput {
    fn default() -> Self {
        Self {
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
            modifiers: Modifiers::default(),
            key_events: Vec::new(),
            typed_text: String::new(),
        }
    }
}

impl PendingInput {
    fn snapshot(&self) -> InputSnapshot {
        InputSnapshot {
            mouse_x: self.mouse_x,
            mouse_y: self.mouse_y,
            mouse_wheel_x: self.mouse_wheel_x,
            mouse_wheel_y: self.mouse_wheel_y,
            mouse_pressed: self.mouse_pressed,
            mouse_down: self.mouse_down,
            mouse_released: self.mouse_released,
            touches: self.touches,
            escape_pressed: self.escape_pressed,
            space_pressed: self.space_pressed,
            space_down: self.space_down,
            f3_pressed: self.f3_pressed,
            r_pressed: self.r_pressed,
            up_pressed: self.up_pressed,
            down_pressed: self.down_pressed,
            modifiers: self.modifiers,
            key_events: self.key_events.clone(),
            typed_text: self.typed_text.clone(),
        }
    }

    fn clear_transient(&mut self) {
        for touch in &mut self.touches {
            match touch {
                Some(point) => match point.phase {
                    TouchPhase::Started | TouchPhase::Moved => point.phase = TouchPhase::Stationary,
                    TouchPhase::Ended | TouchPhase::Cancelled => *touch = None,
                    TouchPhase::Stationary => {}
                },
                None => {}
            }
        }
        self.mouse_wheel_x = 0.0;
        self.mouse_wheel_y = 0.0;
        self.mouse_pressed = false;
        self.mouse_released = false;
        self.escape_pressed = false;
        self.space_pressed = false;
        self.f3_pressed = false;
        self.r_pressed = false;
        self.up_pressed = false;
        self.down_pressed = false;
        self.key_events.clear();
        self.typed_text.clear();
    }
}

impl Default for HostSharedState {
    fn default() -> Self {
        Self {
            latest_frame: HostFrame {
                timing: FrameTiming {
                    delta_seconds: 1.0 / 60.0,
                },
                surface: SurfaceInfo {
                    width: 1280.0,
                    height: 720.0,
                },
                input: PendingInput::default().snapshot(),
            },
            pending_input: PendingInput::default(),
            safe_area_insets: (0.0, 0.0, 0.0, 0.0),
            frame_epoch: 0,
            last_frame_instant: Instant::now(),
            running: true,
            simulate_mouse_with_touch: true,
            pending_redraw: true,
            queued_commands: Vec::new(),
            queued_font_source: None,
            last_submitted_commands: Vec::new(),
            last_submitted_font_source: None,
            next_texture_id: 0,
            last_backend_used: DesktopRenderBackendKind::Unavailable,
            backend_detail: "iOS Metal host waiting for the first frame".to_string(),
            event_proxy: None,
        }
    }
}

static HOST_SHARED: OnceLock<IosHostShared> = OnceLock::new();
static IOS_RUNTIME_ENV: OnceLock<Result<IosRuntimeEnvironment, String>> = OnceLock::new();
const IOS_TOUCH_BRIDGE_ID: u64 = u64::MAX;
static IOS_TOUCH_CONTENT_VIEW: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug)]
struct IosRuntimeEnvironment {
    bundle_root: PathBuf,
    writable_root: PathBuf,
}

fn shared() -> &'static IosHostShared {
    HOST_SHARED.get().expect("ios host not initialized")
}

pub fn configure_runtime_environment() -> Result<(), String> {
    let result = IOS_RUNTIME_ENV.get_or_init(initialize_runtime_environment);
    result.clone().map(|_| ())
}

fn runtime_environment() -> Result<IosRuntimeEnvironment, String> {
    let result = IOS_RUNTIME_ENV.get_or_init(initialize_runtime_environment);
    result.clone()
}

fn initialize_runtime_environment() -> Result<IosRuntimeEnvironment, String> {
    let executable = env::current_exe()
        .map_err(|err| format!("failed to resolve iOS executable path: {err}"))?;
    let bundle_root = executable
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "failed to resolve iOS app bundle root".to_string())?;

    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| "HOME is unavailable in the iOS sandbox".to_string())?;
    let writable_root = home
        .join("Library")
        .join("Application Support")
        .join("sng-rusty");
    std::fs::create_dir_all(&writable_root).map_err(|err| {
        format!(
            "failed to create iOS writable root {}: {err}",
            writable_root.display()
        )
    })?;

    let sng_assets_root = bundle_root.join("assets");
    let loadngo_assets_root = bundle_root.join("loadngo").join("assets");
    let app_icon = sng_assets_root.join("icon").join("app_icon_master.png");

    env::set_current_dir(&bundle_root).map_err(|err| {
        format!(
            "failed to set iOS current directory to {}: {err}",
            bundle_root.display()
        )
    })?;
    unsafe {
        env::set_var("SNG_WRITABLE_ROOT", &writable_root);
        env::set_var("SNG_ASSETS_ROOT", &sng_assets_root);
        env::set_var("LOADNGO_ASSETS_ROOT", &loadngo_assets_root);
        env::set_var("SNG_APP_ICON", &app_icon);
    }

    Ok(IosRuntimeEnvironment {
        bundle_root,
        writable_root,
    })
}

fn lock_state() -> std::sync::MutexGuard<'static, HostSharedState> {
    shared().state.0.lock().expect("ios host state poisoned")
}

fn update_backend_detail(state: &mut HostSharedState, detail: impl Into<String>) {
    let detail = detail.into();
    if state.backend_detail != detail {
        eprintln!("[loadngo/ios] {detail}");
        state.backend_detail = detail;
    }
}

fn trace_input_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        env::var("LOADNGO_TRACE_INPUT")
            .map(|value| {
                let value = value.trim().to_ascii_lowercase();
                matches!(value.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(false)
    })
}

fn trace_input_log(message: impl AsRef<str>) {
    static LOG_COUNT: AtomicUsize = AtomicUsize::new(0);
    if !trace_input_enabled() {
        return;
    }
    let count = LOG_COUNT.fetch_add(1, Ordering::Relaxed);
    if count < 200 {
        eprintln!("[loadngo-input] {}", message.as_ref());
    }
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct LoadngoTouchBridgeTarget;

    impl LoadngoTouchBridgeTarget {
        #[unsafe(method(handlePress:))]
        fn handle_press(&self, recognizer: &UILongPressGestureRecognizer) {
            let phase = match recognizer.state() {
                UIGestureRecognizerState::Began => TouchPhase::Started,
                UIGestureRecognizerState::Changed => TouchPhase::Moved,
                UIGestureRecognizerState::Ended => TouchPhase::Ended,
                UIGestureRecognizerState::Cancelled | UIGestureRecognizerState::Failed => {
                    TouchPhase::Cancelled
                }
                _ => return,
            };
            let content_view_ptr = IOS_TOUCH_CONTENT_VIEW.load(Ordering::Relaxed) as *mut UIView;
            let location = if let Some(content_view) = unsafe { content_view_ptr.as_ref() } {
                if let Some(source_view) = recognizer.view() {
                    let source_point = recognizer.locationInView(Some(&source_view));
                    source_view.convertPoint_toView(source_point, Some(content_view))
                } else {
                    recognizer.locationInView(Some(content_view))
                }
            } else {
                recognizer.locationInView(None)
            };
            trace_input_log(format!(
                "uikit bridge phase={:?} logical=({}, {})",
                phase, location.x, location.y
            ));
            apply_touch_point(TouchPoint {
                id: IOS_TOUCH_BRIDGE_ID,
                x: location.x as f32,
                y: location.y as f32,
                phase,
            });
            wake_runtime_after_input_update();
        }
    }
);

struct IosTouchBridge {
    _target: Retained<LoadngoTouchBridgeTarget>,
    _recognizers: Vec<Retained<UILongPressGestureRecognizer>>,
}

impl IosTouchBridge {
    fn install(view: &UIView) -> Self {
        let mtm = MainThreadMarker::new().expect("iOS touch bridge requires main thread");
        let target: Retained<LoadngoTouchBridgeTarget> =
            unsafe { msg_send_id![LoadngoTouchBridgeTarget::alloc(mtm), init] };
        let mut recognizers = Vec::new();
        IOS_TOUCH_CONTENT_VIEW.store(view as *const UIView as usize, Ordering::Relaxed);

        view.setUserInteractionEnabled(true);
        trace_input_log(format!(
            "installing bridge on view userInteractionEnabled={}",
            view.isUserInteractionEnabled()
        ));
        recognizers.push(Self::install_recognizer(&target, view, mtm));

        if let Some(superview) = view.superview() {
            superview.setUserInteractionEnabled(true);
            trace_input_log(format!(
                "installing bridge on superview userInteractionEnabled={}",
                superview.isUserInteractionEnabled()
            ));
            recognizers.push(Self::install_recognizer(&target, &superview, mtm));
        }

        if let Some(window) = view.window() {
            window.setUserInteractionEnabled(true);
            trace_input_log(format!(
                "installing bridge on window userInteractionEnabled={}",
                window.isUserInteractionEnabled()
            ));
            recognizers.push(Self::install_recognizer(&target, &window, mtm));
        } else {
            trace_input_log("bridge install could not resolve UIWindow from content view");
        }

        Self {
            _target: target,
            _recognizers: recognizers,
        }
    }

    fn install_recognizer(
        target: &LoadngoTouchBridgeTarget,
        host: &UIView,
        mtm: MainThreadMarker,
    ) -> Retained<UILongPressGestureRecognizer> {
        let recognizer = unsafe {
            UILongPressGestureRecognizer::initWithTarget_action(
                mtm.alloc(),
                Some(target as &AnyObject),
                Some(sel!(handlePress:)),
            )
        };
        recognizer.setMinimumPressDuration(0.0);
        recognizer.setAllowableMovement(1024.0);
        recognizer.setNumberOfTouchesRequired(1);
        recognizer.setCancelsTouchesInView(false);
        recognizer.setDelaysTouchesBegan(false);
        recognizer.setDelaysTouchesEnded(false);
        host.addGestureRecognizer(&recognizer);
        recognizer
    }
}

fn advance_frame_clock(state: &mut HostSharedState) {
    let now = Instant::now();
    let dt = now.saturating_duration_since(state.last_frame_instant);
    state.last_frame_instant = now;
    state.latest_frame = HostFrame {
        timing: FrameTiming {
            delta_seconds: dt.as_secs_f32().max(1.0 / 240.0),
        },
        surface: state.latest_frame.surface,
        input: state.pending_input.snapshot(),
    };
    state.frame_epoch = state.frame_epoch.saturating_add(1);
    state.pending_redraw = true;
}

fn apply_touch_point(point: TouchPoint) {
    let mut state = lock_state();
    trace_input_log(format!(
        "host touch id={} phase={:?} logical=({}, {})",
        point.id, point.phase, point.x, point.y
    ));
    set_touch_point(&mut state.pending_input.touches, point);
    if state.simulate_mouse_with_touch {
        state.pending_input.mouse_x = point.x;
        state.pending_input.mouse_y = point.y;
        match point.phase {
            TouchPhase::Started => {
                state.pending_input.mouse_pressed = true;
                state.pending_input.mouse_down = true;
            }
            TouchPhase::Moved | TouchPhase::Stationary => {}
            TouchPhase::Ended | TouchPhase::Cancelled => {
                state.pending_input.mouse_released = true;
                state.pending_input.mouse_down = false;
            }
        }
    }
}

fn wake_runtime_after_input_update() {
    let (lock, cvar) = &*shared().state;
    let mut state = lock.lock().expect("ios host state poisoned");
    if !state.running {
        return;
    }
    advance_frame_clock(&mut state);
    let proxy = state.event_proxy.clone();
    cvar.notify_all();
    drop(state);
    if let Some(proxy) = proxy {
        let _ = proxy.send_event(IosUserEvent::Wake);
    }
}

pub fn desktop_render_backend_status() -> DesktopRenderBackendStatus {
    let state = lock_state();
    DesktopRenderBackendStatus {
        requested: DesktopRenderBackendKind::Metal,
        last_used: state.last_backend_used,
        metal_initialized: matches!(state.last_backend_used, DesktopRenderBackendKind::Metal),
        metal_surface_bound: matches!(state.last_backend_used, DesktopRenderBackendKind::Metal),
        detail: state.backend_detail.clone(),
    }
}

pub fn safe_area_insets() -> (f32, f32, f32, f32) {
    let state = lock_state();
    state.safe_area_insets
}

pub fn launch(
    window: WindowDescriptor,
    _icon: Option<WindowIconSet>,
    entry: impl Future<Output = ()> + 'static,
) {
    configure_runtime_environment().expect("failed to configure iOS runtime environment");
    let env = runtime_environment().expect("failed to access iOS runtime environment");
    let shared = IosHostShared {
        state: Arc::new((Mutex::new(HostSharedState::default()), Condvar::new())),
    };
    let _ = HOST_SHARED.set(shared.clone());

    let event_loop = EventLoop::<IosUserEvent>::with_user_event()
        .build()
        .expect("failed to create iOS event loop");
    {
        let mut state = lock_state();
        state.event_proxy = Some(event_loop.create_proxy());
        update_backend_detail(
            &mut state,
            format!(
                "iOS host ready; bundle_root={} writable_root={}",
                env.bundle_root.display(),
                env.writable_root.display()
            ),
        );
    }
    let mut app = IosApp::new(window, shared, Box::pin(entry));
    let _ = event_loop.run_app(&mut app);
}

pub fn capture_frame() -> HostFrame {
    let mut state = lock_state();
    let frame = state.latest_frame.clone();
    state.pending_input.clear_transient();
    state.latest_frame.input = state.pending_input.snapshot();
    frame
}

pub async fn next_frame(demand: FrameDemand) {
    struct NextFrameFuture {
        demand: FrameDemand,
        observed_epoch: u64,
        waiting_registered: bool,
        timer_registered: bool,
    }

    impl Future for NextFrameFuture {
        type Output = ();

        fn poll(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            let (lock, _cvar) = &*shared().state;
            let state = lock.lock().expect("ios host state poisoned");
            if !state.running {
                return std::task::Poll::Pending;
            }
            if state.frame_epoch > self.observed_epoch {
                self.observed_epoch = state.frame_epoch;
                return std::task::Poll::Ready(());
            }
            drop(state);
            if !self.waiting_registered {
                self.waiting_registered = true;
                let waker = cx.waker().clone();
                let state_arc = shared().state.clone();
                thread::spawn(move || {
                    let (lock, cvar) = &*state_arc;
                    let guard = lock.lock().expect("ios host state poisoned");
                    let _guard = cvar.wait(guard).expect("ios host wait poisoned");
                    waker.wake();
                });
            }
            if !self.timer_registered {
                if let FrameDemand::After(delay) = self.demand {
                    self.timer_registered = true;
                    let waker = cx.waker().clone();
                    let state_arc = shared().state.clone();
                    thread::spawn(move || {
                        thread::sleep(delay);
                        let (lock, cvar) = &*state_arc;
                        let mut state = lock.lock().expect("ios host state poisoned");
                        if !state.running {
                            return;
                        }
                        advance_frame_clock(&mut state);
                        let proxy = state.event_proxy.clone();
                        cvar.notify_all();
                        drop(state);
                        if let Some(proxy) = proxy {
                            let _ = proxy.send_event(IosUserEvent::Wake);
                        }
                        waker.wake();
                    });
                }
            }
            std::task::Poll::Pending
        }
    }

    let observed_epoch = lock_state().frame_epoch;
    NextFrameFuture {
        demand,
        observed_epoch,
        waiting_registered: false,
        timer_registered: false,
    }
    .await;
}

pub fn simulate_mouse_with_touch(enabled: bool) {
    let mut state = lock_state();
    state.simulate_mouse_with_touch = enabled;
}

pub async fn load_bytes(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|err| format!("failed to read {path}: {err}"))
}

pub async fn load_text(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|err| format!("failed to read text {path}: {err}"))
}

pub fn asset_exists(path: &str) -> bool {
    Path::new(path).exists()
}

pub fn set_text_cursor_active(_active: bool) {}

pub fn read_clipboard_text() -> Result<Option<String>, String> {
    Ok(None)
}

pub fn write_clipboard_text(_text: &str) -> Result<(), String> {
    Ok(())
}

pub async fn load_font(path: &str) -> Result<DesktopFont, String> {
    std::fs::metadata(path).map_err(|err| format!("failed to stat font {path}: {err}"))?;
    Ok(DesktopFont::new(Some(path.to_string())))
}

pub fn measure_text_metrics(
    text: &str,
    font: Option<&DesktopFont>,
    font_size: u16,
    font_scale: f32,
) -> TextMetrics {
    metal_measure_text_metrics(
        text,
        font.and_then(DesktopFont::source_path),
        font_size as f32 * font_scale,
    )
    .unwrap_or(TextMetrics {
        width: text.chars().count() as f32 * font_size as f32 * font_scale * 0.6,
        height: font_size as f32 * font_scale,
    })
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
    let line_height = ui_core::multiline_line_step(font_size) * font_scale.max(0.0);
    let line_box_height = ui_core::single_line_text_box_height(font_size) * font_scale.max(0.0);
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
    let renderer = Renderer::new(RendererConfig::default());
    let commands = renderer.encode_render_ops(ops);
    render_commands(&commands, font);
}

pub fn render_widget_paint_ops(ops: &[PaintOp]) {
    let renderer = Renderer::new(RendererConfig::default());
    let commands = renderer.encode_paint_ops(ops);
    render_commands(&commands, None);
}

pub fn clear(color: UiColor) {
    render_commands(&[FrameCommand::Clear { color }], None);
}

pub fn draw_plain_text(text: &str, x: f32, y: f32, size: f32, color: UiColor) -> TextMetrics {
    let (font_size, font_scale) = font_size_and_scale(size);
    let metrics = measure_text_metrics(text, None, font_size, font_scale);
    let line_box_height = ui_core::single_line_text_box_height(font_size) * font_scale.max(0.0);
    render_commands(
        &[FrameCommand::Text(loadngo_renderer::TextRequest {
            rect: UiRect {
                x,
                y,
                width: metrics.width.max(1.0),
                height: line_box_height,
            },
            clip_rect: None,
            text: text.to_string(),
            style: RenderTextStyle {
                color,
                font_size,
                horizontal_align: loadngo_host_core::RenderTextHorizontalAlign::Left,
                vertical_align: loadngo_host_core::RenderTextVerticalAlign::Top,
                vertical_metric_mode:
                    loadngo_host_core::RenderTextVerticalMetricMode::LogicalLineBox,
                layout_mode: loadngo_host_core::RenderTextLayoutMode::SingleLine,
                overflow: loadngo_host_core::RenderTextOverflow::Clip,
            },
            font_source: None,
            direction: loadngo_renderer::TextDirection::Auto,
            script: loadngo_renderer::TextScript::Auto,
            language: None,
        })],
        None,
    );
    metrics
}

pub fn blit_texture(texture: &DesktopTexture, rect: UiRect, alpha: f32) {
    render_commands(
        &[FrameCommand::Image(ImageRequest {
            image_key: texture.image_key.clone(),
            rect,
            clip_rect: None,
            alpha,
        })],
        None,
    );
}

pub fn upload_texture(image: &DecodedImage) -> Result<DesktopTexture, String> {
    upload_texture_with_image_key(None, image)
}

pub fn upload_texture_with_image_key(
    image_key: Option<&str>,
    image: &DecodedImage,
) -> Result<DesktopTexture, String> {
    image.validate_rgba8()?;
    let image_key = match image_key {
        Some(key) => key.to_string(),
        None => {
            let mut state = lock_state();
            state.next_texture_id = state.next_texture_id.saturating_add(1);
            format!("__loadngo_ios_texture_{}", state.next_texture_id)
        }
    };
    register_image_resource(&image_key, image);
    Ok(DesktopTexture::new(
        image_key,
        image.width as f32,
        image.height as f32,
    ))
}

pub fn draw_texture_fit(texture: &DesktopTexture, x: f32, y: f32, width: f32, height: f32) {
    let scale = (width / texture.width())
        .min(height / texture.height())
        .max(0.01);
    let draw_w = texture.width() * scale;
    let draw_h = texture.height() * scale;
    let draw_x = x + (width - draw_w) * 0.5;
    let draw_y = y + (height - draw_h) * 0.5;
    blit_texture(
        texture,
        UiRect {
            x: draw_x,
            y: draw_y,
            width: draw_w,
            height: draw_h,
        },
        1.0,
    );
}

pub fn draw_rectangle(x: f32, y: f32, w: f32, h: f32, color: UiColor) {
    render_commands(
        &[FrameCommand::FillRect {
            rect: UiRect {
                x,
                y,
                width: w,
                height: h,
            },
            color,
        }],
        None,
    );
}

pub fn draw_rectangle_lines(x: f32, y: f32, w: f32, h: f32, thickness: f32, color: UiColor) {
    render_commands(
        &[FrameCommand::StrokeRect {
            rect: UiRect {
                x,
                y,
                width: w,
                height: h,
            },
            color,
            thickness: thickness.max(1.0).round() as i32,
        }],
        None,
    );
}

pub fn draw_text(text: &str, x: f32, y: f32, size: f32, color: UiColor) {
    let _ = draw_plain_text(text, x, y, size, color);
}

pub fn measure_text(text: &str, _font: Option<()>, font_size: u16, font_scale: f32) -> TextMetrics {
    measure_text_metrics(text, None, font_size, font_scale)
}

fn render_commands(commands: &[FrameCommand], font: Option<&DesktopFont>) {
    if commands.is_empty() {
        return;
    }
    let mut state = lock_state();
    state.queued_commands.extend_from_slice(commands);
    if let Some(source) = font.and_then(DesktopFont::source_path) {
        state.queued_font_source = Some(source.to_string());
    }
    state.pending_redraw = true;
    let proxy = state.event_proxy.clone();
    drop(state);
    if let Some(proxy) = proxy {
        let _ = proxy.send_event(IosUserEvent::Wake);
    }
}

fn font_size_and_scale(size: f32) -> (u16, f32) {
    let clamped = size.max(1.0);
    let font_size = clamped.round().min(u16::MAX as f32) as u16;
    let font_scale = (clamped / font_size as f32).max(0.01);
    (font_size, font_scale)
}

struct IosApp {
    descriptor: WindowDescriptor,
    shared: IosHostShared,
    entry: Option<Pin<Box<dyn Future<Output = ()>>>>,
    pool: LocalPool,
    window: Option<Window>,
    window_id: Option<WindowId>,
    metal_backend: Option<MetalBackend>,
    touch_bridge: Option<IosTouchBridge>,
}

impl IosApp {
    fn new(
        descriptor: WindowDescriptor,
        shared: IosHostShared,
        entry: Pin<Box<dyn Future<Output = ()>>>,
    ) -> Self {
        Self {
            descriptor,
            shared,
            entry: Some(entry),
            pool: LocalPool::new(),
            window: None,
            window_id: None,
            metal_backend: None,
            touch_bridge: None,
        }
    }

    fn publish_frame(&mut self) {
        let (lock, cvar) = &*self.shared.state;
        let mut state = lock.lock().expect("ios host state poisoned");
        advance_frame_clock(&mut state);
        cvar.notify_all();
    }

    fn request_redraw_if_needed(&self) {
        if let Some(window) = &self.window {
            let state = lock_state();
            if state.pending_redraw {
                window.request_redraw();
            }
        }
    }

    fn flush_selected_backend(&mut self) {
        let (commands, font_source) = {
            let mut state = lock_state();
            let commands = if state.queued_commands.is_empty() {
                state.last_submitted_commands.clone()
            } else {
                let commands = std::mem::take(&mut state.queued_commands);
                state.last_submitted_commands = commands.clone();
                commands
            };
            let font_source = if state.queued_font_source.is_some() {
                let font_source = state.queued_font_source.take();
                state.last_submitted_font_source = font_source.clone();
                font_source
            } else {
                state.last_submitted_font_source.clone()
            };
            (commands, font_source)
        };

        if commands.is_empty() {
            return;
        }

        let Some(backend) = self.metal_backend.as_mut() else {
            let mut state = lock_state();
            update_backend_detail(
                &mut state,
                "iOS Metal backend unavailable because no surface is bound",
            );
            return;
        };

        backend.set_text_font_source(font_source.as_deref());
        match Renderer::new(RendererConfig::default()).render(backend, &commands) {
            Ok(()) => {
                let mut state = lock_state();
                state.last_backend_used = DesktopRenderBackendKind::Metal;
                state.pending_redraw = false;
                update_backend_detail(&mut state, "iOS Metal backend rendered the queued frame");
            }
            Err(err) => {
                let mut state = lock_state();
                update_backend_detail(&mut state, format!("iOS Metal render failed: {err}"));
            }
        }
    }

    fn sync_surface_info_from_backend(&mut self, fallback_size: (f32, f32)) {
        let (width, height) = self
            .metal_backend
            .as_ref()
            .and_then(MetalBackend::surface_logical_size_points)
            .unwrap_or(fallback_size);
        let mut state = lock_state();
        state.latest_frame.surface = SurfaceInfo { width, height };
    }

    fn refresh_safe_area_insets(&mut self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let handle = match window.window_handle() {
            Ok(handle) => handle,
            Err(_) => return,
        };
        let raw_handle = handle.as_raw();
        let view: *mut AnyObject = match raw_handle {
            RawWindowHandle::UiKit(handle) => handle.ui_view.as_ptr().cast(),
            _ => return,
        };
        let view = unsafe { &*view.cast::<UIView>() };
        let insets = view.safeAreaInsets();
        let mut state = lock_state();
        state.safe_area_insets = (
            insets.left as f32,
            insets.top as f32,
            insets.right as f32,
            insets.bottom as f32,
        );
        trace_input_log(format!(
            "safe area insets left={} top={} right={} bottom={}",
            state.safe_area_insets.0,
            state.safe_area_insets.1,
            state.safe_area_insets.2,
            state.safe_area_insets.3
        ));
    }
}

impl ApplicationHandler<IosUserEvent> for IosApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            self.request_redraw_if_needed();
            return;
        }

        let attrs = WindowAttributes::default().with_title(self.descriptor.title.clone());
        let window = event_loop
            .create_window(attrs)
            .expect("failed to create iOS window");
        let size = window.inner_size();

        let handle = window
            .window_handle()
            .expect("failed to access iOS raw window handle");
        let raw_handle = handle.as_raw();
        let view: *mut AnyObject = match raw_handle {
            RawWindowHandle::UiKit(handle) => handle.ui_view.as_ptr().cast(),
            other => panic!("unexpected iOS window handle: {other:?}"),
        };
        let view_ref = unsafe { &*view.cast::<UIView>() };
        self.touch_bridge = Some(IosTouchBridge::install(view_ref));

        let mut metal_backend =
            MetalBackend::try_bind_system_default().expect("failed to create Metal device");
        metal_backend
            .try_bind_host_view_surface(view)
            .expect("failed to bind Metal layer to UIKit view");

        self.window_id = Some(window.id());
        self.metal_backend = Some(metal_backend);
        self.window = Some(window);
        self.refresh_safe_area_insets();
        self.sync_surface_info_from_backend((size.width as f32, size.height as f32));
        {
            let mut state = lock_state();
            let surface = state.latest_frame.surface;
            update_backend_detail(
                &mut state,
                format!(
                    "iOS Metal backend initialized and surface-bound (surface={}x{})",
                    surface.width, surface.height
                ),
            );
        }

        if let Some(entry) = self.entry.take() {
            let shared = self.shared.clone();
            self.pool
                .spawner()
                .spawn_local(async move {
                    entry.await;
                    let (lock, cvar) = &*shared.state;
                    let mut state = lock.lock().expect("ios host state poisoned");
                    state.running = false;
                    cvar.notify_all();
                })
                .expect("failed to spawn iOS runtime future");
        }

        self.publish_frame();
        self.request_redraw_if_needed();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let mut should_publish_frame = false;
        match event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => {
                let (lock, cvar) = &*self.shared.state;
                let mut state = lock.lock().expect("ios host state poisoned");
                state.running = false;
                cvar.notify_all();
                drop(state);
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                self.flush_selected_backend();
            }
            WindowEvent::Resized(size) => {
                self.refresh_safe_area_insets();
                self.sync_surface_info_from_backend((size.width as f32, size.height as f32));
                let mut state = lock_state();
                state.pending_redraw = true;
                let surface = state.latest_frame.surface;
                trace_input_log(format!(
                    "window resized physical={}x{} logical={}x{}",
                    size.width, size.height, surface.width, surface.height
                ));
                update_backend_detail(
                    &mut state,
                    format!(
                        "iOS surface resized to {}x{}",
                        surface.width, surface.height
                    ),
                );
                should_publish_frame = true;
            }
            WindowEvent::Touch(touch) => {
                let scale_factor = self
                    .window
                    .as_ref()
                    .map(Window::scale_factor)
                    .unwrap_or(1.0);
                trace_input_log(format!(
                    "winit touch id={} phase={:?} physical=({}, {}) scale={}",
                    touch.id, touch.phase, touch.location.x, touch.location.y, scale_factor
                ));
                if self.touch_bridge.is_none() {
                    apply_touch_event(touch, scale_factor);
                }
                should_publish_frame = true;
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                let mut state = lock_state();
                if let Some(filtered) = sanitized_typed_text(&text) {
                    state.pending_input.typed_text.push_str(&filtered);
                }
                should_publish_frame = true;
            }
            WindowEvent::ModifiersChanged(mods) => {
                let mut state = lock_state();
                state.pending_input.modifiers = Modifiers {
                    shift: mods.state().shift_key(),
                    ctrl: mods.state().control_key(),
                    alt: mods.state().alt_key(),
                    meta: mods.state().super_key(),
                };
                should_publish_frame = true;
            }
            WindowEvent::KeyboardInput { event, .. } => {
                handle_keyboard_input(event);
                should_publish_frame = true;
            }
            _ => {}
        }
        if should_publish_frame {
            self.publish_frame();
        }
        self.request_redraw_if_needed();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.pool.run_until_stalled();
        {
            let state = lock_state();
            if !state.running {
                drop(state);
                event_loop.exit();
                return;
            }
        }
        self.request_redraw_if_needed();
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: IosUserEvent) {
        self.request_redraw_if_needed();
    }
}

fn handle_keyboard_input(event: winit::event::KeyEvent) {
    let pressed = event.state == ElementState::Pressed;
    let mut state = lock_state();
    let modifiers = state.pending_input.modifiers;

    match event.physical_key {
        PhysicalKey::Code(KeyCode::Escape) if pressed => state.pending_input.escape_pressed = true,
        PhysicalKey::Code(KeyCode::Space) => {
            state.pending_input.space_down = pressed;
            if pressed {
                state.pending_input.space_pressed = true;
            }
        }
        PhysicalKey::Code(KeyCode::F3) if pressed => state.pending_input.f3_pressed = true,
        PhysicalKey::Code(KeyCode::KeyR) if pressed => state.pending_input.r_pressed = true,
        PhysicalKey::Code(KeyCode::ArrowUp) if pressed => state.pending_input.up_pressed = true,
        PhysicalKey::Code(KeyCode::ArrowDown) if pressed => state.pending_input.down_pressed = true,
        _ => {}
    }

    if pressed {
        if !modifiers.ctrl && !modifiers.meta {
            if let Some(text) = event.text.as_ref() {
                if let Some(filtered) = sanitized_typed_text(text.as_str()) {
                    state.pending_input.typed_text.push_str(&filtered);
                }
            }
        }
        if let Some(host_key) = map_host_key(&event) {
            state.pending_input.key_events.push(HostKeyEvent {
                key: host_key,
                modifiers,
            });
        }
    }
}

fn map_host_key(event: &winit::event::KeyEvent) -> Option<HostKey> {
    match &event.logical_key {
        WinitKey::Named(NamedKey::Escape) => Some(HostKey::Escape),
        WinitKey::Named(NamedKey::Space) => Some(HostKey::Space),
        WinitKey::Named(NamedKey::ArrowUp) => Some(HostKey::Up),
        WinitKey::Named(NamedKey::ArrowDown) => Some(HostKey::Down),
        WinitKey::Named(NamedKey::ArrowLeft) => Some(HostKey::Left),
        WinitKey::Named(NamedKey::ArrowRight) => Some(HostKey::Right),
        WinitKey::Named(NamedKey::Home) => Some(HostKey::Home),
        WinitKey::Named(NamedKey::End) => Some(HostKey::End),
        WinitKey::Named(NamedKey::Enter) => Some(HostKey::Enter),
        WinitKey::Named(NamedKey::Tab) => Some(HostKey::Tab),
        WinitKey::Named(NamedKey::Backspace) => Some(HostKey::Backspace),
        WinitKey::Named(NamedKey::Delete) => Some(HostKey::Delete),
        WinitKey::Character(text) => match text.to_ascii_uppercase().as_str() {
            "A" => Some(HostKey::A),
            "C" => Some(HostKey::C),
            "F" => Some(HostKey::F),
            "R" => Some(HostKey::R),
            "S" => Some(HostKey::S),
            "V" => Some(HostKey::V),
            "Y" => Some(HostKey::Y),
            "Z" => Some(HostKey::Z),
            _ => None,
        },
        _ => match event.physical_key {
            PhysicalKey::Code(KeyCode::F3) => Some(HostKey::F3),
            _ => None,
        },
    }
}

fn apply_touch_event(touch: WinitTouch, scale_factor: f64) {
    let scale = scale_factor.max(1.0) as f32;
    apply_touch_point(TouchPoint {
        id: touch.id,
        x: touch.location.x as f32 / scale,
        y: touch.location.y as f32 / scale,
        phase: map_touch_phase(touch.phase),
    });
}

fn map_touch_phase(phase: WinitTouchPhase) -> TouchPhase {
    match phase {
        WinitTouchPhase::Started => TouchPhase::Started,
        WinitTouchPhase::Moved => TouchPhase::Moved,
        WinitTouchPhase::Ended => TouchPhase::Ended,
        WinitTouchPhase::Cancelled => TouchPhase::Cancelled,
    }
}

fn set_touch_point(touches: &mut [Option<TouchPoint>; 8], point: TouchPoint) {
    if let Some(existing) = touches
        .iter_mut()
        .find(|slot| slot.as_ref().is_some_and(|touch| touch.id == point.id))
    {
        *existing = Some(point);
        return;
    }
    if let Some(empty) = touches.iter_mut().find(|slot| slot.is_none()) {
        *empty = Some(point);
        return;
    }
    touches[0] = Some(point);
}

fn sanitized_typed_text(text: &str) -> Option<String> {
    let filtered: String = text.chars().filter(|ch| !ch.is_control()).collect();
    if filtered.is_empty() {
        None
    } else {
        Some(filtered)
    }
}
