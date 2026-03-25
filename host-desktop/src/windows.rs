#![allow(dead_code)]

use std::collections::{hash_map::DefaultHasher, HashMap};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::Instant;

use arboard::Clipboard;
use fontdue::{Font, FontSettings};
use futures::executor::LocalPool;
use futures::task::LocalSpawnExt;
use loadngo_gfx_dx12::Dx12Backend;
use loadngo_host_core::{
    DecodedImage, FrameDemand, FrameTiming, HostFrame, HostKey, HostKeyEvent, ImageRegistry,
    InputSnapshot, RenderOp, RenderTextHorizontalAlign, RenderTextLayoutMode, RenderTextOverflow,
    RenderTextStyle, RenderTextVerticalAlign, RenderTextVerticalMetricMode, SurfaceInfo,
    TextMetrics, WindowDescriptor, WindowIconSet,
};
use loadngo_renderer::{FrameCommand, ImageRequest, Renderer, RendererConfig, TextRequest};
use softbuffer::{Context, Surface};
use ui_core::{
    geometry::{Color as UiColor, Point, Rect as UiRect},
    paint::PaintOp,
    Modifiers,
};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WinitKey, KeyCode, NamedKey, PhysicalKey};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Icon, Window, WindowAttributes, WindowId};

#[derive(Clone)]
pub struct DesktopFont {
    source_path: Option<String>,
    font: Arc<Font>,
}

impl DesktopFont {
    fn new(source_path: Option<String>, font: Font) -> Self {
        Self {
            source_path,
            font: Arc::new(font),
        }
    }

    pub fn source_path(&self) -> Option<&str> {
        self.source_path.as_deref()
    }
}

#[derive(Clone)]
pub struct DesktopTexture {
    image_key: Option<String>,
    width: f32,
    height: f32,
    image: Arc<DecodedImage>,
}

impl DesktopTexture {
    fn new(image_key: Option<String>, image: DecodedImage) -> Self {
        Self {
            image_key,
            width: image.width as f32,
            height: image.height as f32,
            image: Arc::new(image),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopRenderBackendKind {
    D3d12,
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

#[derive(Clone)]
struct WindowsHostShared {
    state: Arc<(Mutex<HostSharedState>, Condvar)>,
}

#[derive(Debug, Clone, Copy)]
enum WindowsUserEvent {
    Wake,
}

struct HostSharedState {
    latest_frame: HostFrame,
    pending_input: PendingInput,
    frame_epoch: u64,
    last_frame_instant: Instant,
    dpi_scale: f32,
    running: bool,
    simulate_mouse_with_touch: bool,
    cursor_visible: bool,
    clear_color: UiColor,
    commands: Vec<FrameCommand>,
    image_registry: ImageRegistry,
    textures: HashMap<String, Arc<DecodedImage>>,
    generated_texture_cache: HashMap<String, Arc<DecodedImage>>,
    pending_redraw: bool,
    last_backend_used: DesktopRenderBackendKind,
    backend_detail: String,
    event_proxy: Option<EventLoopProxy<WindowsUserEvent>>,
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
            touches: [None; 8],
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
            frame_epoch: 0,
            last_frame_instant: Instant::now(),
            dpi_scale: 1.0,
            running: true,
            simulate_mouse_with_touch: false,
            cursor_visible: true,
            clear_color: UiColor::rgba(0, 0, 0, 0xff),
            commands: Vec::new(),
            image_registry: ImageRegistry::new(),
            textures: HashMap::new(),
            generated_texture_cache: HashMap::new(),
            pending_redraw: true,
            last_backend_used: DesktopRenderBackendKind::Unavailable,
            backend_detail: "Windows host waiting for the first frame".to_string(),
            event_proxy: None,
        }
    }
}

static HOST_SHARED: OnceLock<WindowsHostShared> = OnceLock::new();
static DEFAULT_FONT: OnceLock<DesktopFont> = OnceLock::new();
static FONT_CACHE: OnceLock<Mutex<HashMap<String, DesktopFont>>> = OnceLock::new();
static TRACE_ENABLED: OnceLock<bool> = OnceLock::new();

fn shared() -> &'static WindowsHostShared {
    HOST_SHARED.get().expect("windows host not initialized")
}

fn lock_state() -> std::sync::MutexGuard<'static, HostSharedState> {
    shared()
        .state
        .0
        .lock()
        .expect("windows host state poisoned")
}

fn font_cache() -> &'static Mutex<HashMap<String, DesktopFont>> {
    FONT_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn trace_enabled() -> bool {
    *TRACE_ENABLED.get_or_init(|| match std::env::var("LOADNGO_WINDOWS_TRACE") {
        Ok(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            !matches!(normalized.as_str(), "" | "0" | "false" | "no" | "off")
        }
        Err(_) => false,
    })
}

fn trace_windows(message: impl AsRef<str>) {
    if trace_enabled() {
        eprintln!("[loadngo/windows/trace] {}", message.as_ref());
    }
}

fn sanitized_typed_text(text: &str) -> Option<String> {
    let filtered: String = text.chars().filter(|ch| !ch.is_control()).collect();
    if filtered.is_empty() {
        None
    } else {
        Some(filtered)
    }
}

fn init_default_font() -> DesktopFont {
    let candidates = [
        r"C:\Windows\Fonts\segoeui.ttf",
        r"C:\Windows\Fonts\arial.ttf",
        r"C:\Windows\Fonts\tahoma.ttf",
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(font) = Font::from_bytes(bytes, FontSettings::default()) {
                return DesktopFont::new(Some(path.to_string()), font);
            }
        }
    }
    panic!("failed to load a usable Windows default font");
}

fn default_font() -> &'static DesktopFont {
    DEFAULT_FONT.get_or_init(init_default_font)
}

fn load_font_from_path(path: &str) -> Result<DesktopFont, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("failed to read font {path}: {err}"))?;
    let font = Font::from_bytes(bytes, FontSettings::default())
        .map_err(|err| format!("failed to parse font {path}: {err}"))?;
    Ok(DesktopFont::new(Some(path.to_string()), font))
}

fn decode_icon(icon: WindowIconSet) -> Option<Icon> {
    Icon::from_rgba(icon.medium_rgba8, 32, 32).ok()
}

fn requested_render_backend() -> DesktopRenderBackendKind {
    match std::env::var("LOADNGO_DESKTOP_BACKEND")
        .ok()
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("d3d12") | None => DesktopRenderBackendKind::D3d12,
        Some("software") => DesktopRenderBackendKind::Software,
        _ => DesktopRenderBackendKind::Unavailable,
    }
}

fn describe_unsupported_dx12_command(commands: &[FrameCommand]) -> Option<&'static str> {
    commands.iter().find_map(|command| match command {
        FrameCommand::Clear { .. } => None,
        FrameCommand::FillRect { .. } => None,
        FrameCommand::StrokeRect { .. } => None,
        FrameCommand::Image(_) => None,
        FrameCommand::Text(_) => Some("Text"),
        FrameCommand::Line { .. } => Some("Line"),
        FrameCommand::Circle { .. } => Some("Circle"),
        FrameCommand::Polyline { .. } => Some("Polyline"),
        FrameCommand::ParticleBatch { .. } => Some("ParticleBatch"),
    })
}

fn update_backend_detail(state: &mut HostSharedState, detail: impl Into<String>) {
    let detail = detail.into();
    if state.backend_detail != detail {
        eprintln!("[loadngo/windows] {detail}");
        state.backend_detail = detail;
    }
}

fn scale_from_window(window: &Window, high_dpi: bool) -> f32 {
    if high_dpi {
        window.scale_factor().max(1.0) as f32
    } else {
        1.0
    }
}

fn logical_surface_info(size: PhysicalSize<u32>, scale: f32) -> SurfaceInfo {
    SurfaceInfo {
        width: size.width as f32 / scale.max(1.0),
        height: size.height as f32 / scale.max(1.0),
    }
}

fn logical_cursor_position(position: PhysicalPosition<f64>, scale: f32) -> (f32, f32) {
    (
        position.x as f32 / scale.max(1.0),
        position.y as f32 / scale.max(1.0),
    )
}

pub fn desktop_render_backend_status() -> DesktopRenderBackendStatus {
    let state = lock_state();
    DesktopRenderBackendStatus {
        requested: requested_render_backend(),
        last_used: state.last_backend_used,
        metal_initialized: false,
        metal_surface_bound: matches!(state.last_backend_used, DesktopRenderBackendKind::D3d12),
        detail: state.backend_detail.clone(),
    }
}

pub fn launch(
    window: WindowDescriptor,
    icon: Option<WindowIconSet>,
    entry: impl Future<Output = ()> + 'static,
) {
    let shared = WindowsHostShared {
        state: Arc::new((Mutex::new(HostSharedState::default()), Condvar::new())),
    };
    let _ = HOST_SHARED.set(shared.clone());

    let event_loop = EventLoop::<WindowsUserEvent>::with_user_event()
        .build()
        .expect("failed to create Windows event loop");
    {
        let mut state = lock_state();
        state.event_proxy = Some(event_loop.create_proxy());
    }
    let mut app = WindowsApp::new(window, icon, shared, Box::pin(entry));
    let _ = event_loop.run_app(&mut app);
}

pub fn capture_frame() -> HostFrame {
    let mut state = lock_state();
    let frame = state.latest_frame.clone();
    state.pending_input.clear_transient();
    state.latest_frame.input.mouse_wheel_x = 0.0;
    state.latest_frame.input.mouse_wheel_y = 0.0;
    state.latest_frame.input.mouse_pressed = false;
    state.latest_frame.input.mouse_released = false;
    state.latest_frame.input.escape_pressed = false;
    state.latest_frame.input.space_pressed = false;
    state.latest_frame.input.f3_pressed = false;
    state.latest_frame.input.r_pressed = false;
    state.latest_frame.input.up_pressed = false;
    state.latest_frame.input.down_pressed = false;
    state.latest_frame.input.key_events.clear();
    state.latest_frame.input.typed_text.clear();
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
            let state = lock.lock().expect("windows host state poisoned");
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
                    let guard = lock.lock().expect("windows host state poisoned");
                    let _guard = cvar.wait(guard).expect("windows host wait poisoned");
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
                        let mut state = lock.lock().expect("windows host state poisoned");
                        if !state.running {
                            return;
                        }
                        advance_frame_clock(&mut state);
                        let proxy = state.event_proxy.clone();
                        cvar.notify_all();
                        drop(state);
                        if let Some(proxy) = proxy {
                            let _ = proxy.send_event(WindowsUserEvent::Wake);
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

pub fn set_text_cursor_active(active: bool) {
    let mut state = lock_state();
    state.cursor_visible = !active;
    state.pending_redraw = true;
}

pub fn read_clipboard_text() -> Result<Option<String>, String> {
    let mut clipboard = Clipboard::new().map_err(|err| format!("clipboard unavailable: {err}"))?;
    match clipboard.get_text() {
        Ok(text) => Ok(Some(text)),
        Err(arboard::Error::ContentNotAvailable) => Ok(None),
        Err(err) => Err(format!("failed to read clipboard text: {err}")),
    }
}

pub fn write_clipboard_text(text: &str) -> Result<(), String> {
    let mut clipboard = Clipboard::new().map_err(|err| format!("clipboard unavailable: {err}"))?;
    clipboard
        .set_text(text.to_string())
        .map_err(|err| format!("failed to write clipboard text: {err}"))
}

pub async fn load_font(path: &str) -> Result<DesktopFont, String> {
    load_font_from_path(path)
}

pub fn measure_text_metrics(
    text: &str,
    font: Option<&DesktopFont>,
    font_size: u16,
    font_scale: f32,
) -> TextMetrics {
    measure_text_impl(text, font.unwrap_or(default_font()), font_size, font_scale)
}

pub fn wrap_text_lines(
    text: &str,
    font: Option<&DesktopFont>,
    font_size: u16,
    font_scale: f32,
    max_width: f32,
) -> Vec<String> {
    let font = font.unwrap_or(default_font());
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
            let candidate = format!("{current_line} {word}");
            if measure_text_impl(&candidate, font, font_size, font_scale).width <= max_width {
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
    let line_step = ui_core::multiline_line_step(font_size) * font_scale.max(0.01);
    let line_box_height = ui_core::single_line_text_box_height(font_size) * font_scale.max(0.01);
    let mut ops = Vec::new();
    let mut current_y = y;
    for line in lines {
        ops.push(RenderOp::Text {
            rect: UiRect {
                x,
                y: current_y,
                width: 4096.0,
                height: line_box_height,
            },
            text: line.clone(),
            style: RenderTextStyle {
                color,
                font_size,
                horizontal_align: RenderTextHorizontalAlign::Left,
                vertical_align: RenderTextVerticalAlign::Top,
                layout_mode: RenderTextLayoutMode::SingleLine,
                overflow: RenderTextOverflow::Clip,
                ..RenderTextStyle::default()
            },
        });
        current_y += line_step + line_spacing;
    }
    render_ops(&ops, font);
}

pub fn render_ops(ops: &[RenderOp], _font: Option<&DesktopFont>) {
    let mut commands = Renderer::new(RendererConfig::default()).encode_render_ops(ops);
    if let Some(font_source) = _font.and_then(DesktopFont::source_path).map(str::to_string) {
        apply_font_source_to_commands(&mut commands, &font_source);
    }
    let mut state = lock_state();
    state.commands.extend(commands);
    state.pending_redraw = true;
}

pub fn render_widget_paint_ops(ops: &[PaintOp]) {
    let commands = Renderer::new(RendererConfig::default()).encode_paint_ops(ops);
    let mut state = lock_state();
    state.commands.extend(commands);
    state.pending_redraw = true;
}

pub fn clear(color: UiColor) {
    let mut state = lock_state();
    state.clear_color = color;
    state.commands.clear();
    state.commands.push(FrameCommand::Clear { color });
    state.pending_redraw = true;
}

pub fn draw_plain_text(text: &str, x: f32, y: f32, size: f32, color: UiColor) -> TextMetrics {
    let font_size = size.max(1.0).round() as u16;
    let metrics = measure_text_metrics(text, None, font_size, 1.0);
    render_ops(
        &[RenderOp::Text {
            rect: UiRect {
                x,
                y,
                width: metrics.width.ceil(),
                height: metrics.height.ceil(),
            },
            text: text.to_string(),
            style: RenderTextStyle {
                color,
                font_size,
                ..RenderTextStyle::default()
            },
        }],
        None,
    );
    metrics
}

pub fn blit_texture(texture: &DesktopTexture, rect: UiRect, alpha: f32) {
    let mut state = lock_state();
    let key = texture
        .image_key
        .clone()
        .unwrap_or_else(|| format!("anon:{}", Arc::as_ptr(&texture.image) as usize));
    state.textures.insert(key.clone(), texture.image.clone());
    state.commands.push(FrameCommand::Image(ImageRequest {
        rect,
        clip_rect: None,
        image_key: key,
        alpha,
    }));
    state.pending_redraw = true;
}

pub fn upload_texture(image: &DecodedImage) -> Result<DesktopTexture, String> {
    upload_texture_with_image_key(None, image)
}

pub fn upload_texture_with_image_key(
    image_key: Option<&str>,
    image: &DecodedImage,
) -> Result<DesktopTexture, String> {
    image.validate_rgba8()?;
    let texture = DesktopTexture::new(image_key.map(str::to_string), image.clone());
    if let Some(image_key) = image_key {
        let mut state = lock_state();
        state
            .image_registry
            .insert(image_key.to_string(), image.clone());
        state
            .textures
            .insert(image_key.to_string(), Arc::new(image.clone()));
    }
    Ok(texture)
}

pub fn draw_texture_fit(texture: &DesktopTexture, x: f32, y: f32, width: f32, height: f32) {
    blit_texture(
        texture,
        UiRect {
            x,
            y,
            width,
            height,
        },
        1.0,
    );
}

pub fn draw_rectangle(x: f32, y: f32, w: f32, h: f32, color: UiColor) {
    render_ops(
        &[RenderOp::FillRect {
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
    render_ops(
        &[RenderOp::StrokeRect {
            rect: UiRect {
                x,
                y,
                width: w,
                height: h,
            },
            color,
            thickness: thickness.max(1.0) as i32,
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

#[derive(Clone, Copy)]
struct SoftwareTextLineLayout {
    px: f32,
    line_height: f32,
    baseline_offset: f32,
}

fn software_text_line_layout(
    font: &DesktopFont,
    font_size: u16,
    font_scale: f32,
) -> SoftwareTextLineLayout {
    let px = (font_size as f32 * font_scale.max(0.01)).max(1.0);
    if let Some(metrics) = font.font.horizontal_line_metrics(px) {
        return SoftwareTextLineLayout {
            px,
            line_height: metrics.new_line_size.max(px),
            baseline_offset: metrics.ascent.max(0.0),
        };
    }

    SoftwareTextLineLayout {
        px,
        line_height: px,
        baseline_offset: px * 0.8,
    }
}

fn measure_text_impl(
    text: &str,
    font: &DesktopFont,
    font_size: u16,
    font_scale: f32,
) -> TextMetrics {
    let layout = software_text_line_layout(font, font_size, font_scale);
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
        let metrics = font.font.metrics(ch, layout.px);
        current_width += metrics.advance_width.max(metrics.width as f32);
    }
    max_width = max_width.max(current_width);
    TextMetrics {
        width: max_width,
        height: layout.line_height.max(1.0) * line_count as f32,
    }
}

struct WindowsApp {
    descriptor: WindowDescriptor,
    icon: Option<WindowIconSet>,
    shared: WindowsHostShared,
    entry: Option<Pin<Box<dyn Future<Output = ()>>>>,
    pool: LocalPool,
    window: Option<Arc<Window>>,
    window_id: Option<WindowId>,
    surface: Option<Surface<Arc<Window>, Arc<Window>>>,
    dx12_backend: Option<Dx12Backend>,
}

impl WindowsApp {
    fn new(
        descriptor: WindowDescriptor,
        icon: Option<WindowIconSet>,
        shared: WindowsHostShared,
        entry: Pin<Box<dyn Future<Output = ()>>>,
    ) -> Self {
        Self {
            descriptor,
            icon,
            shared,
            entry: Some(entry),
            pool: LocalPool::new(),
            window: None,
            window_id: None,
            surface: None,
            dx12_backend: None,
        }
    }

    fn publish_frame(&mut self) {
        let (lock, cvar) = &*self.shared.state;
        let mut state = lock.lock().expect("windows host state poisoned");
        advance_frame_clock(&mut state);
        cvar.notify_all();
    }

    fn shutdown_graphics(&mut self) {
        trace_windows(format!(
            "shutdown_graphics surface={} dx12={} window={}",
            self.surface.is_some(),
            self.dx12_backend.is_some(),
            self.window.is_some()
        ));
        self.dx12_backend = None;
        self.surface = None;
        self.window_id = None;
        self.window = None;
    }

    fn request_redraw_if_needed(&self) {
        if let Some(window) = &self.window {
            let state = lock_state();
            if state.pending_redraw {
                window.request_redraw();
            }
        }
    }
}

impl ApplicationHandler<WindowsUserEvent> for WindowsApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let mut attrs = WindowAttributes::default()
            .with_title(self.descriptor.title.clone())
            .with_inner_size(LogicalSize::new(
                self.descriptor.width.unwrap_or(1280) as f64,
                self.descriptor.height.unwrap_or(720) as f64,
            ));
        if let Some(icon) = self.icon.clone().and_then(decode_icon) {
            attrs = attrs.with_window_icon(Some(icon));
        }

        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("failed to create Windows window"),
        );
        let context = Context::new(window.clone()).expect("failed to create softbuffer context");
        let surface =
            Surface::new(&context, window.clone()).expect("failed to create softbuffer surface");
        let size = window.inner_size();
        let dpi_scale = scale_from_window(&window, self.descriptor.high_dpi);
        let requested_backend = requested_render_backend();

        {
            let mut state = lock_state();
            state.dpi_scale = dpi_scale;
            state.latest_frame.surface = logical_surface_info(size, dpi_scale);
            match requested_backend {
                DesktopRenderBackendKind::D3d12 => update_backend_detail(
                    &mut state,
                    format!(
                        "Windows D3D12 backend requested; attempting native swapchain binding (dpi_scale={dpi_scale:.2})"
                    ),
                ),
                DesktopRenderBackendKind::Software => update_backend_detail(
                    &mut state,
                    format!("Windows software renderer active (dpi_scale={dpi_scale:.2})"),
                ),
                DesktopRenderBackendKind::Unavailable => update_backend_detail(
                    &mut state,
                    format!(
                        "Windows backend selection unavailable; using software renderer (dpi_scale={dpi_scale:.2})"
                    ),
                ),
            }
        }

        if matches!(requested_backend, DesktopRenderBackendKind::D3d12) {
            let hwnd = match window.window_handle().map(|handle| handle.as_raw()) {
                Ok(RawWindowHandle::Win32(handle)) => Some(handle.hwnd.get()),
                _ => None,
            };
            match hwnd.map(|hwnd| Dx12Backend::try_bind_hwnd(hwnd, size.width as i32, size.height as i32)) {
                Some(Ok(backend)) => {
                    self.dx12_backend = Some(backend);
                    let mut state = lock_state();
                    update_backend_detail(
                        &mut state,
                        format!(
                            "Windows D3D12 backend bound to the native window (dpi_scale={dpi_scale:.2})"
                        ),
                    );
                }
                Some(Err(err)) => {
                    let mut state = lock_state();
                    update_backend_detail(
                        &mut state,
                        format!("Windows D3D12 backend unavailable: {err}; falling back to software"),
                    );
                }
                None => {
                    let mut state = lock_state();
                    update_backend_detail(
                        &mut state,
                        "Windows D3D12 backend unavailable: failed to obtain a Win32 window handle; falling back to software",
                    );
                }
            }
        }

        self.window_id = Some(window.id());
        self.surface = Some(surface);
        self.window = Some(window);
        if let Some(entry) = self.entry.take() {
            let shared = self.shared.clone();
            self.pool
                .spawner()
                .spawn_local(async move {
                    entry.await;
                    let (lock, cvar) = &*shared.state;
                    let mut state = lock.lock().expect("windows host state poisoned");
                    state.running = false;
                    cvar.notify_all();
                })
                .expect("failed to spawn Windows runtime future");
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
            WindowEvent::CloseRequested => {
                trace_windows("window_event CloseRequested");
                let (lock, cvar) = &*self.shared.state;
                let mut state = lock.lock().expect("windows host state poisoned");
                state.running = false;
                cvar.notify_all();
                drop(state);
                self.shutdown_graphics();
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                if let (Some(window), Some(surface)) = (&self.window, &mut self.surface) {
                    present(
                        surface,
                        window.inner_size(),
                        scale_from_window(window, self.descriptor.high_dpi),
                        &mut self.dx12_backend,
                    );
                }
                let mut state = lock_state();
                state.pending_redraw = false;
            }
            WindowEvent::Resized(size) => {
                let mut state = lock_state();
                let dpi_scale = self
                    .window
                    .as_ref()
                    .map(|window| scale_from_window(window, self.descriptor.high_dpi))
                    .unwrap_or(1.0);
                if let Some(backend) = self.dx12_backend.as_mut() {
                    backend.update_surface_size(size.width as i32, size.height as i32);
                }
                state.dpi_scale = dpi_scale;
                state.latest_frame.surface = logical_surface_info(size, dpi_scale);
                state.pending_redraw = true;
                should_publish_frame = true;
            }
            WindowEvent::CursorMoved { position, .. } => {
                let mut state = lock_state();
                let (x, y) = logical_cursor_position(position, state.dpi_scale);
                state.pending_input.mouse_x = x;
                state.pending_input.mouse_y = y;
                should_publish_frame = true;
            }
            WindowEvent::MouseInput {
                state: button_state,
                button,
                ..
            } => {
                if button == MouseButton::Left {
                    let mut state = lock_state();
                    match button_state {
                        ElementState::Pressed => {
                            state.pending_input.mouse_pressed = true;
                            state.pending_input.mouse_down = true;
                        }
                        ElementState::Released => {
                            state.pending_input.mouse_released = true;
                            state.pending_input.mouse_down = false;
                        }
                    }
                    should_publish_frame = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let mut state = lock_state();
                match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        state.pending_input.mouse_wheel_x += x;
                        state.pending_input.mouse_wheel_y += y;
                    }
                    MouseScrollDelta::PixelDelta(PhysicalPosition { x, y }) => {
                        let scale = state.dpi_scale.max(1.0);
                        state.pending_input.mouse_wheel_x += x as f32 / (24.0 * scale);
                        state.pending_input.mouse_wheel_y += y as f32 / (24.0 * scale);
                    }
                }
                should_publish_frame = true;
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let mut state = lock_state();
                state.dpi_scale = if self.descriptor.high_dpi {
                    scale_factor.max(1.0) as f32
                } else {
                    1.0
                };
                if let Some(window) = &self.window {
                    if let Some(backend) = self.dx12_backend.as_mut() {
                        let size = window.inner_size();
                        backend.update_surface_size(size.width as i32, size.height as i32);
                    }
                    state.latest_frame.surface =
                        logical_surface_info(window.inner_size(), state.dpi_scale);
                }
                state.pending_redraw = true;
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
                self.shutdown_graphics();
                event_loop.exit();
                return;
            }
        }
        self.request_redraw_if_needed();
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: WindowsUserEvent) {
        self.request_redraw_if_needed();
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

fn present(
    surface: &mut Surface<Arc<Window>, Arc<Window>>,
    size: PhysicalSize<u32>,
    dpi_scale: f32,
    dx12_backend: &mut Option<Dx12Backend>,
) {
    let width = size.width.max(1);
    let height = size.height.max(1);

    let (clear_color, commands, textures, generated_cache) = {
        let state = lock_state();
        (
            state.clear_color,
            state.commands.clone(),
            state.textures.clone(),
            state.generated_texture_cache.clone(),
        )
    };
    let commands = scale_frame_commands(commands, dpi_scale);

    if let Some(backend) = dx12_backend.as_mut() {
        let (dx12_commands, dx12_textures, next_generated_cache) =
            prepare_dx12_frame(&commands, &textures, generated_cache);
        {
            let mut state = lock_state();
            state.generated_texture_cache = next_generated_cache;
        }
        backend.update_surface_size(width as i32, height as i32);
        backend.sync_image_resources(
            dx12_textures
                .iter()
                .map(|(key, image)| (key.clone(), image.clone())),
        );
        if backend.supports_commands(&dx12_commands) {
            match Renderer::new(RendererConfig::default()).render(backend, &dx12_commands) {
                Ok(()) => {
                    let mut state = lock_state();
                    state.last_backend_used = DesktopRenderBackendKind::D3d12;
                    update_backend_detail(
                        &mut state,
                        "Windows D3D12 backend rendered the queued frame",
                    );
                    return;
                }
                Err(err) => {
                    *dx12_backend = None;
                    let mut state = lock_state();
                    update_backend_detail(
                        &mut state,
                        format!(
                            "Windows D3D12 render failed, disabling the backend and falling back to software: {err}"
                        ),
                    );
                }
            }
        } else {
            let mut state = lock_state();
            let reason = describe_unsupported_dx12_command(&dx12_commands).unwrap_or("Unknown");
            update_backend_detail(
                &mut state,
                format!(
                    "Windows D3D12 backend rejected queued frame because it contains unsupported {reason} commands; falling back to software"
                ),
            );
        }
    } else {
        let mut state = lock_state();
        state.generated_texture_cache = generated_cache;
    }

    surface
        .resize(
            std::num::NonZeroU32::new(width).expect("nonzero width"),
            std::num::NonZeroU32::new(height).expect("nonzero height"),
        )
        .expect("failed to resize Windows surface");

    let mut buffer = surface
        .buffer_mut()
        .expect("failed to lock Windows surface buffer");
    let mut rgba = vec![0u8; width as usize * height as usize * 4];

    fill_rect_rgba(
        &mut rgba,
        width as usize,
        height as usize,
        UiRect {
            x: 0.0,
            y: 0.0,
            width: width as f32,
            height: height as f32,
        },
        clear_color,
    );

    for command in commands {
        match command {
            FrameCommand::Clear { color } => fill_rect_rgba(
                &mut rgba,
                width as usize,
                height as usize,
                UiRect {
                    x: 0.0,
                    y: 0.0,
                    width: width as f32,
                    height: height as f32,
                },
                color,
            ),
            FrameCommand::FillRect { rect, color } => {
                fill_rect_rgba(&mut rgba, width as usize, height as usize, rect, color);
            }
            FrameCommand::StrokeRect {
                rect,
                color,
                thickness,
            } => {
                stroke_rect_rgba(
                    &mut rgba,
                    width as usize,
                    height as usize,
                    rect,
                    color,
                    thickness.max(1) as usize,
                );
            }
            FrameCommand::Line {
                from,
                to,
                color,
                thickness,
            } => draw_line_rgba(
                &mut rgba,
                width as usize,
                height as usize,
                from,
                to,
                color,
                thickness.max(1),
            ),
            FrameCommand::Circle {
                center,
                radius,
                color,
            } => fill_circle_rgba(
                &mut rgba,
                width as usize,
                height as usize,
                center,
                radius.max(1),
                color,
            ),
            FrameCommand::Polyline {
                points,
                color,
                thickness,
                closed,
            } => draw_polyline_rgba(
                &mut rgba,
                width as usize,
                height as usize,
                points.as_slice(),
                color,
                thickness.max(1),
                closed,
            ),
            FrameCommand::ParticleBatch { particles } => {
                for particle in particles {
                    fill_circle_rgba(
                        &mut rgba,
                        width as usize,
                        height as usize,
                        particle.center,
                        particle.radius.round().max(1.0) as i32,
                        particle.color,
                    );
                }
            }
            FrameCommand::Text(request) => {
                draw_text_request(&mut rgba, width as usize, height as usize, &request)
            }
            FrameCommand::Image(request) => {
                if let Some(image) = textures.get(&request.image_key) {
                    blit_image_rgba(&mut rgba, width as usize, height as usize, image, &request);
                }
            }
        }
    }

    for (dst, chunk) in buffer.iter_mut().zip(rgba.chunks_exact(4)) {
        *dst = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
    }
    buffer.present().expect("failed to present Windows surface");
    let mut state = lock_state();
    state.last_backend_used = DesktopRenderBackendKind::Software;
    update_backend_detail(
        &mut state,
        "Windows software renderer rendered the queued frame",
    );
}

fn prepare_dx12_frame(
    commands: &[FrameCommand],
    textures: &HashMap<String, Arc<DecodedImage>>,
    mut generated_cache: HashMap<String, Arc<DecodedImage>>,
) -> (
    Vec<FrameCommand>,
    HashMap<String, Arc<DecodedImage>>,
    HashMap<String, Arc<DecodedImage>>,
) {
    let mut next_commands = Vec::with_capacity(commands.len());
    let mut next_textures = textures.clone();
    let mut generated_index = 0usize;

    for command in commands {
        match command {
            FrameCommand::Text(request) => {
                let image_key = generated_text_cache_key(request);
                if !generated_cache.contains_key(&image_key) {
                    if let Some((_, image)) = rasterize_text_command(request) {
                        generated_cache.insert(image_key.clone(), Arc::new(image));
                    }
                }
                if let Some(image) = generated_cache.get(&image_key) {
                    let draw_rect =
                        rasterized_text_draw_rect(request, image.width as f32, image.height as f32);
                    let clip_rect = request
                        .clip_rect
                        .and_then(|clip| intersect_rects(clip, request.rect))
                        .or(Some(request.rect));
                    next_textures.insert(image_key.clone(), image.clone());
                    next_commands.push(FrameCommand::Image(ImageRequest {
                        rect: draw_rect,
                        clip_rect,
                        image_key,
                        alpha: 1.0,
                    }));
                } else {
                    next_commands.push(command.clone());
                }
            }
            FrameCommand::Line {
                from,
                to,
                color,
                thickness,
            } => {
                if let Some((image_key, rect, image)) =
                    rasterize_line_command(*from, *to, *color, *thickness, generated_index)
                {
                    generated_index += 1;
                    next_textures.insert(image_key.clone(), Arc::new(image));
                    next_commands.push(FrameCommand::Image(ImageRequest {
                        rect,
                        clip_rect: None,
                        image_key,
                        alpha: 1.0,
                    }));
                } else {
                    next_commands.push(command.clone());
                }
            }
            FrameCommand::Circle {
                center,
                radius,
                color,
            } => {
                if let Some((image_key, rect, image)) =
                    rasterize_circle_command(*center, *radius, *color, generated_index)
                {
                    generated_index += 1;
                    next_textures.insert(image_key.clone(), Arc::new(image));
                    next_commands.push(FrameCommand::Image(ImageRequest {
                        rect,
                        clip_rect: None,
                        image_key,
                        alpha: 1.0,
                    }));
                } else {
                    next_commands.push(command.clone());
                }
            }
            FrameCommand::Polyline {
                points,
                color,
                thickness,
                closed,
            } => {
                append_rasterized_polyline_images(
                    &mut next_commands,
                    &mut next_textures,
                    points.as_slice(),
                    *color,
                    *thickness,
                    *closed,
                    &mut generated_index,
                );
            }
            FrameCommand::ParticleBatch { particles } => {
                append_rasterized_particle_images(
                    &mut next_commands,
                    &mut next_textures,
                    particles,
                    &mut generated_index,
                );
            }
            _ => next_commands.push(command.clone()),
        }
    }

    (next_commands, next_textures, generated_cache)
}

fn scale_frame_commands(commands: Vec<FrameCommand>, scale: f32) -> Vec<FrameCommand> {
    if (scale - 1.0).abs() <= f32::EPSILON {
        return commands;
    }
    commands
        .into_iter()
        .map(|command| scale_frame_command(command, scale))
        .collect()
}

fn scale_frame_command(command: FrameCommand, scale: f32) -> FrameCommand {
    match command {
        FrameCommand::Clear { color } => FrameCommand::Clear { color },
        FrameCommand::FillRect { rect, color } => FrameCommand::FillRect {
            rect: scale_rect(rect, scale),
            color,
        },
        FrameCommand::StrokeRect {
            rect,
            color,
            thickness,
        } => FrameCommand::StrokeRect {
            rect: scale_rect(rect, scale),
            color,
            thickness: scale_i32(thickness, scale),
        },
        FrameCommand::Line {
            from,
            to,
            color,
            thickness,
        } => FrameCommand::Line {
            from: scale_point(from, scale),
            to: scale_point(to, scale),
            color,
            thickness: scale_i32(thickness, scale),
        },
        FrameCommand::Circle {
            center,
            radius,
            color,
        } => FrameCommand::Circle {
            center: scale_point(center, scale),
            radius: scale_i32(radius, scale),
            color,
        },
        FrameCommand::Polyline {
            points,
            color,
            thickness,
            closed,
        } => FrameCommand::Polyline {
            points: points
                .into_iter()
                .map(|point| scale_point(point, scale))
                .collect(),
            color,
            thickness: scale_i32(thickness, scale),
            closed,
        },
        FrameCommand::ParticleBatch { particles } => FrameCommand::ParticleBatch {
            particles: particles
                .into_iter()
                .map(|mut particle| {
                    particle.center = scale_point(particle.center, scale);
                    particle.radius *= scale;
                    particle
                })
                .collect(),
        },
        FrameCommand::Text(request) => FrameCommand::Text(scale_text_request(request, scale)),
        FrameCommand::Image(request) => FrameCommand::Image(scale_image_request(request, scale)),
    }
}

fn scale_point(point: Point, scale: f32) -> Point {
    Point {
        x: point.x * scale,
        y: point.y * scale,
    }
}

fn scale_rect(rect: UiRect, scale: f32) -> UiRect {
    UiRect {
        x: rect.x * scale,
        y: rect.y * scale,
        width: rect.width * scale,
        height: rect.height * scale,
    }
}

fn scale_i32(value: i32, scale: f32) -> i32 {
    ((value.max(1) as f32) * scale).round().max(1.0) as i32
}

fn scale_font_size(font_size: u16, scale: f32) -> u16 {
    ((font_size.max(1) as f32) * scale)
        .round()
        .clamp(1.0, u16::MAX as f32) as u16
}

fn scale_text_request(mut request: TextRequest, scale: f32) -> TextRequest {
    request.rect = scale_rect(request.rect, scale);
    request.clip_rect = request.clip_rect.map(|clip| scale_rect(clip, scale));
    request.style.font_size = scale_font_size(request.style.font_size, scale);
    request
}

fn scale_image_request(mut request: ImageRequest, scale: f32) -> ImageRequest {
    request.rect = scale_rect(request.rect, scale);
    request.clip_rect = request.clip_rect.map(|clip| scale_rect(clip, scale));
    request
}

fn generated_text_cache_key(request: &TextRequest) -> String {
    let mut hasher = DefaultHasher::new();
    request.text.hash(&mut hasher);
    request.font_source.hash(&mut hasher);
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

fn rasterized_text_draw_rect(
    request: &TextRequest,
    texture_width: f32,
    texture_height: f32,
) -> UiRect {
    let padding_x = 4.0;
    let padding_y = 6.0;
    UiRect {
        x: request.rect.x - padding_x,
        y: request.rect.y - padding_y,
        width: texture_width,
        height: texture_height,
    }
}

fn intersect_rects(a: UiRect, b: UiRect) -> Option<UiRect> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width).min(b.x + b.width);
    let y1 = (a.y + a.height).min(b.y + b.height);
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

fn rasterize_text_command(request: &TextRequest) -> Option<(UiRect, DecodedImage)> {
    let font = resolve_text_request_font(request);
    let measured = measure_text_impl(&request.text, &font, request.style.font_size, 1.0);
    let padding_x = 4.0;
    let padding_y = 6.0;
    let layout = software_text_line_layout(&font, request.style.font_size, 1.0);
    let line_box_height = ui_core::single_line_text_box_height(request.style.font_size);
    let line_step = ui_core::multiline_line_step(request.style.font_size);
    let line_count = match request.style.layout_mode {
        RenderTextLayoutMode::SingleLine => 1usize,
        RenderTextLayoutMode::MultiLine => request.text.lines().count().max(1),
    };
    let first_line_height = match request.style.vertical_metric_mode {
        RenderTextVerticalMetricMode::LogicalLineBox => line_box_height,
        RenderTextVerticalMetricMode::VisibleInk => layout.line_height.max(1.0),
    };
    let content_height = first_line_height + line_step * line_count.saturating_sub(1) as f32;
    let width = request
        .rect
        .width
        .max(measured.width.ceil() + padding_x * 2.0)
        .max(1.0);
    let height = request
        .rect
        .height
        .max(content_height + padding_y * 2.0)
        .max(measured.height.ceil() + padding_y * 2.0)
        .max(1.0);
    let tex_width = width.ceil() as usize;
    let tex_height = height.ceil() as usize;
    let mut rgba = vec![0u8; tex_width * tex_height * 4];
    let mut local_request = request.clone();
    local_request.rect = UiRect {
        x: padding_x,
        y: padding_y,
        width: (width - padding_x * 2.0).max(1.0),
        height: (height - padding_y * 2.0).max(1.0),
    };
    draw_text_request(&mut rgba, tex_width, tex_height, &local_request);
    Some((
        rasterized_text_draw_rect(request, tex_width as f32, tex_height as f32),
        DecodedImage::new(tex_width as u32, tex_height as u32, rgba),
    ))
}

fn rasterize_line_command(
    from: Point,
    to: Point,
    color: UiColor,
    thickness: i32,
    index: usize,
) -> Option<(String, UiRect, DecodedImage)> {
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
    let tex_width = rect.width.ceil() as usize;
    let tex_height = rect.height.ceil() as usize;
    let mut rgba = vec![0u8; tex_width * tex_height * 4];
    draw_line_rgba(
        &mut rgba,
        tex_width,
        tex_height,
        Point {
            x: from.x - rect.x,
            y: from.y - rect.y,
        },
        Point {
            x: to.x - rect.x,
            y: to.y - rect.y,
        },
        color,
        thickness,
    );
    Some((
        format!("generated://line/{index}"),
        rect,
        DecodedImage::new(tex_width as u32, tex_height as u32, rgba),
    ))
}

fn rasterize_circle_command(
    center: Point,
    radius: i32,
    color: UiColor,
    index: usize,
) -> Option<(String, UiRect, DecodedImage)> {
    if radius <= 0 {
        return None;
    }
    let radius = radius as f32;
    let rect = UiRect {
        x: center.x - radius,
        y: center.y - radius,
        width: radius * 2.0,
        height: radius * 2.0,
    };
    let tex_width = rect.width.max(1.0).ceil() as usize;
    let tex_height = rect.height.max(1.0).ceil() as usize;
    let mut rgba = vec![0u8; tex_width * tex_height * 4];
    fill_circle_rgba(
        &mut rgba,
        tex_width,
        tex_height,
        Point {
            x: radius,
            y: radius,
        },
        radius as i32,
        color,
    );
    Some((
        format!("generated://circle/{index}"),
        rect,
        DecodedImage::new(tex_width as u32, tex_height as u32, rgba),
    ))
}

fn append_rasterized_polyline_images(
    commands: &mut Vec<FrameCommand>,
    textures: &mut HashMap<String, Arc<DecodedImage>>,
    points: &[Point],
    color: UiColor,
    thickness: i32,
    closed: bool,
    generated_index: &mut usize,
) {
    if points.len() < 2 {
        return;
    }
    for segment in points.windows(2) {
        if let Some((image_key, rect, image)) =
            rasterize_line_command(segment[0], segment[1], color, thickness, *generated_index)
        {
            *generated_index += 1;
            textures.insert(image_key.clone(), Arc::new(image));
            commands.push(FrameCommand::Image(ImageRequest {
                rect,
                clip_rect: None,
                image_key,
                alpha: 1.0,
            }));
        }
    }
    if closed {
        if let Some((image_key, rect, image)) = rasterize_line_command(
            *points.last().unwrap_or(&points[0]),
            points[0],
            color,
            thickness,
            *generated_index,
        ) {
            *generated_index += 1;
            textures.insert(image_key.clone(), Arc::new(image));
            commands.push(FrameCommand::Image(ImageRequest {
                rect,
                clip_rect: None,
                image_key,
                alpha: 1.0,
            }));
        }
    }
}

fn append_rasterized_particle_images(
    commands: &mut Vec<FrameCommand>,
    textures: &mut HashMap<String, Arc<DecodedImage>>,
    particles: &[ui_core::Particle],
    generated_index: &mut usize,
) {
    for particle in particles {
        if let Some((image_key, rect, image)) = rasterize_circle_command(
            particle.center,
            particle.radius.round().max(1.0) as i32,
            particle.color,
            *generated_index,
        ) {
            *generated_index += 1;
            textures.insert(image_key.clone(), Arc::new(image));
            commands.push(FrameCommand::Image(ImageRequest {
                rect,
                clip_rect: None,
                image_key,
                alpha: 1.0,
            }));
        }
    }
}

fn fill_rect_rgba(buffer: &mut [u8], width: usize, height: usize, rect: UiRect, color: UiColor) {
    let x0 = rect.x.max(0.0) as usize;
    let y0 = rect.y.max(0.0) as usize;
    let x1 = (rect.x + rect.width).max(0.0).min(width as f32) as usize;
    let y1 = (rect.y + rect.height).max(0.0).min(height as f32) as usize;
    for y in y0..y1 {
        for x in x0..x1 {
            blend_pixel(buffer, width, x, y, color, 1.0);
        }
    }
}

fn stroke_rect_rgba(
    buffer: &mut [u8],
    width: usize,
    height: usize,
    rect: UiRect,
    color: UiColor,
    thickness: usize,
) {
    let thickness_f = thickness as f32;
    fill_rect_rgba(
        buffer,
        width,
        height,
        UiRect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: thickness_f,
        },
        color,
    );
    fill_rect_rgba(
        buffer,
        width,
        height,
        UiRect {
            x: rect.x,
            y: rect.y + rect.height - thickness_f,
            width: rect.width,
            height: thickness_f,
        },
        color,
    );
    fill_rect_rgba(
        buffer,
        width,
        height,
        UiRect {
            x: rect.x,
            y: rect.y,
            width: thickness_f,
            height: rect.height,
        },
        color,
    );
    fill_rect_rgba(
        buffer,
        width,
        height,
        UiRect {
            x: rect.x + rect.width - thickness_f,
            y: rect.y,
            width: thickness_f,
            height: rect.height,
        },
        color,
    );
}

fn draw_line_rgba(
    buffer: &mut [u8],
    width: usize,
    height: usize,
    from: Point,
    to: Point,
    color: UiColor,
    thickness: i32,
) {
    let mut x0 = from.x.round() as i32;
    let mut y0 = from.y.round() as i32;
    let x1 = to.x.round() as i32;
    let y1 = to.y.round() as i32;
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;

    loop {
        for oy in -(thickness / 2)..=(thickness / 2) {
            for ox in -(thickness / 2)..=(thickness / 2) {
                let px = x0 + ox;
                let py = y0 + oy;
                if px >= 0 && py >= 0 && (px as usize) < width && (py as usize) < height {
                    blend_pixel(buffer, width, px as usize, py as usize, color, 1.0);
                }
            }
        }
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
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

fn draw_polyline_rgba(
    buffer: &mut [u8],
    width: usize,
    height: usize,
    points: &[Point],
    color: UiColor,
    thickness: i32,
    closed: bool,
) {
    if points.len() < 2 {
        return;
    }
    for segment in points.windows(2) {
        draw_line_rgba(
            buffer, width, height, segment[0], segment[1], color, thickness,
        );
    }
    if closed {
        draw_line_rgba(
            buffer,
            width,
            height,
            *points.last().unwrap_or(&points[0]),
            points[0],
            color,
            thickness,
        );
    }
}

fn fill_circle_rgba(
    buffer: &mut [u8],
    width: usize,
    height: usize,
    center: Point,
    radius: i32,
    color: UiColor,
) {
    let center_x = center.x.round() as i32;
    let center_y = center.y.round() as i32;
    let r2 = radius * radius;
    for y in (center_y - radius)..=(center_y + radius) {
        for x in (center_x - radius)..=(center_x + radius) {
            let dx = x - center_x;
            let dy = y - center_y;
            if dx * dx + dy * dy <= r2
                && x >= 0
                && y >= 0
                && (x as usize) < width
                && (y as usize) < height
            {
                blend_pixel(buffer, width, x as usize, y as usize, color, 1.0);
            }
        }
    }
}

fn draw_text_request(buffer: &mut [u8], width: usize, height: usize, request: &TextRequest) {
    let font = resolve_text_request_font(request);
    let clip_rect = request
        .clip_rect
        .and_then(|clip| intersect_rects(clip, request.rect))
        .or(Some(request.rect));
    let layout = software_text_line_layout(&font, request.style.font_size, 1.0);
    let measured_line_height = layout.line_height.max(1.0);
    let line_box_height = ui_core::single_line_text_box_height(request.style.font_size);
    let line_step = ui_core::multiline_line_step(request.style.font_size);
    let baseline_offset = match request.style.vertical_metric_mode {
        RenderTextVerticalMetricMode::LogicalLineBox => {
            layout.baseline_offset + (line_box_height - measured_line_height).max(0.0) * 0.5
        }
        RenderTextVerticalMetricMode::VisibleInk => layout.baseline_offset,
    };
    let normalized_text = match request.style.layout_mode {
        RenderTextLayoutMode::SingleLine => request.text.replace('\n', " "),
        RenderTextLayoutMode::MultiLine => request.text.clone(),
    };
    let lines: Vec<&str> = normalized_text.split('\n').collect();
    let first_line_height = match request.style.vertical_metric_mode {
        RenderTextVerticalMetricMode::LogicalLineBox => line_box_height,
        RenderTextVerticalMetricMode::VisibleInk => measured_line_height,
    };
    let total_height = first_line_height + line_step * lines.len().saturating_sub(1) as f32;
    let mut origin_y = request.rect.y;
    origin_y += match request.style.vertical_align {
        RenderTextVerticalAlign::Top => 0.0,
        RenderTextVerticalAlign::Middle => (request.rect.height - total_height).max(0.0) * 0.5,
        RenderTextVerticalAlign::Bottom => (request.rect.height - total_height).max(0.0),
    };

    for (line_index, line) in lines.iter().enumerate() {
        let rendered = apply_overflow(
            line,
            &font,
            request.style.font_size,
            request.rect.width,
            request.style.overflow.clone(),
        );
        let metrics = measure_text_impl(&rendered, &font, request.style.font_size, 1.0);
        let start_x = match request.style.horizontal_align {
            RenderTextHorizontalAlign::Left => request.rect.x.round() as i32,
            RenderTextHorizontalAlign::Center => (request.rect.x
                + (request.rect.width - metrics.width).max(0.0) * 0.5)
                .round() as i32,
            RenderTextHorizontalAlign::Right => {
                (request.rect.x + (request.rect.width - metrics.width).max(0.0)).round() as i32
            }
        };
        let baseline_y = origin_y + line_index as f32 * line_step + baseline_offset;
        draw_text_line(
            buffer,
            width,
            height,
            &rendered,
            start_x,
            baseline_y,
            &font,
            request.style.font_size,
            request.style.color,
            clip_rect,
        );
    }
}

fn apply_font_source_to_commands(commands: &mut [FrameCommand], font_source: &str) {
    for command in commands {
        if let FrameCommand::Text(request) = command {
            request.font_source = Some(font_source.to_string());
        }
    }
}

fn resolve_text_request_font(request: &TextRequest) -> DesktopFont {
    let Some(font_source) = request.font_source.as_deref() else {
        return default_font().clone();
    };
    if default_font().source_path() == Some(font_source) {
        return default_font().clone();
    }
    let mut cache = font_cache().lock().expect("windows font cache poisoned");
    if let Some(font) = cache.get(font_source) {
        return font.clone();
    }
    let loaded = load_font_from_path(font_source).unwrap_or_else(|_| default_font().clone());
    cache.insert(font_source.to_string(), loaded.clone());
    loaded
}

fn apply_overflow(
    text: &str,
    font: &DesktopFont,
    font_size: u16,
    max_width: f32,
    overflow: RenderTextOverflow,
) -> String {
    if measure_text_impl(text, font, font_size, 1.0).width <= max_width {
        return text.to_string();
    }
    match overflow {
        RenderTextOverflow::Clip => text.to_string(),
        RenderTextOverflow::EllipsisMiddle => {
            let chars: Vec<char> = text.chars().collect();
            let mut left = chars.len() / 2;
            let mut right = left;
            while left > 0 && right < chars.len() {
                let candidate = format!(
                    "{}...{}",
                    chars[..left].iter().collect::<String>(),
                    chars[right..].iter().collect::<String>()
                );
                if measure_text_impl(&candidate, font, font_size, 1.0).width <= max_width {
                    return candidate;
                }
                left -= 1;
                right += 1;
            }
            "...".to_string()
        }
        RenderTextOverflow::EllipsisEnd => {
            let mut current = text.to_string();
            while !current.is_empty() {
                let candidate = format!("{current}...");
                if measure_text_impl(&candidate, font, font_size, 1.0).width <= max_width {
                    return candidate;
                }
                current.pop();
            }
            "...".to_string()
        }
    }
}

fn draw_text_line(
    buffer: &mut [u8],
    width: usize,
    height: usize,
    text: &str,
    x: i32,
    baseline_y: f32,
    font: &DesktopFont,
    font_size: u16,
    color: UiColor,
    clip_rect: Option<UiRect>,
) {
    let layout = software_text_line_layout(font, font_size, 1.0);
    let mut pen_x = x as f32;
    for ch in text.chars() {
        let (metrics, bitmap) = font.font.rasterize(ch, layout.px);
        let glyph_x = pen_x.round() as i32 + metrics.xmin;
        let glyph_y = (baseline_y - metrics.height as f32 - metrics.ymin as f32).round() as i32;
        for gy in 0..metrics.height {
            for gx in 0..metrics.width {
                let alpha = bitmap[gy * metrics.width + gx] as f32 / 255.0;
                let px = glyph_x + gx as i32;
                let py = glyph_y + gy as i32;
                let inside_clip = clip_rect.is_none_or(|clip| {
                    (px as f32) >= clip.x
                        && (px as f32) < clip.x + clip.width
                        && (py as f32) >= clip.y
                        && (py as f32) < clip.y + clip.height
                });
                if inside_clip
                    && px >= 0
                    && py >= 0
                    && (px as usize) < width
                    && (py as usize) < height
                {
                    blend_pixel(buffer, width, px as usize, py as usize, color, alpha);
                }
            }
        }
        pen_x += metrics.advance_width;
    }
}

fn blit_image_rgba(
    buffer: &mut [u8],
    width: usize,
    height: usize,
    image: &DecodedImage,
    request: &ImageRequest,
) {
    let x0 = request.rect.x.max(0.0) as usize;
    let y0 = request.rect.y.max(0.0) as usize;
    let x1 = (request.rect.x + request.rect.width)
        .max(0.0)
        .min(width as f32) as usize;
    let y1 = (request.rect.y + request.rect.height)
        .max(0.0)
        .min(height as f32) as usize;

    let dst_w = (x1.saturating_sub(x0)).max(1);
    let dst_h = (y1.saturating_sub(y0)).max(1);
    for dy in 0..dst_h {
        let sy = dy * image.height as usize / dst_h;
        for dx in 0..dst_w {
            let sx = dx * image.width as usize / dst_w;
            let src_idx = (sy * image.width as usize + sx) * 4;
            let color = UiColor::rgba(
                image.rgba8[src_idx],
                image.rgba8[src_idx + 1],
                image.rgba8[src_idx + 2],
                image.rgba8[src_idx + 3],
            );
            blend_pixel(
                buffer,
                width,
                x0 + dx,
                y0 + dy,
                color,
                request.alpha.clamp(0.0, 1.0),
            );
        }
    }
}

fn blend_pixel(buffer: &mut [u8], width: usize, x: usize, y: usize, color: UiColor, alpha: f32) {
    let idx = (y * width + x) * 4;
    let src_a = (color.a as f32 / 255.0) * alpha.clamp(0.0, 1.0);
    if src_a <= 0.0 {
        return;
    }
    let dst_a = buffer[idx + 3] as f32 / 255.0;
    let out_a = src_a + dst_a * (1.0 - src_a);
    let src_r = color.r as f32 / 255.0;
    let src_g = color.g as f32 / 255.0;
    let src_b = color.b as f32 / 255.0;
    let dst_r = buffer[idx] as f32 / 255.0;
    let dst_g = buffer[idx + 1] as f32 / 255.0;
    let dst_b = buffer[idx + 2] as f32 / 255.0;
    let out_r = if out_a > 0.0 {
        (src_r * src_a + dst_r * dst_a * (1.0 - src_a)) / out_a
    } else {
        0.0
    };
    let out_g = if out_a > 0.0 {
        (src_g * src_a + dst_g * dst_a * (1.0 - src_a)) / out_a
    } else {
        0.0
    };
    let out_b = if out_a > 0.0 {
        (src_b * src_a + dst_b * dst_a * (1.0 - src_a)) / out_a
    } else {
        0.0
    };
    buffer[idx] = (out_r * 255.0).round().clamp(0.0, 255.0) as u8;
    buffer[idx + 1] = (out_g * 255.0).round().clamp(0.0, 255.0) as u8;
    buffer[idx + 2] = (out_b * 255.0).round().clamp(0.0, 255.0) as u8;
    buffer[idx + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;
    use loadngo_renderer::{
        rgba as renderer_rgba, FrameCommand, TextDirection, TextRequest, TextScript,
    };

    fn sample_text_request() -> TextRequest {
        TextRequest {
            rect: UiRect {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 32.0,
            },
            clip_rect: None,
            text: "Custom font".to_string(),
            style: RenderTextStyle {
                color: renderer_rgba(255, 255, 255, 255),
                font_size: 18,
                horizontal_align: RenderTextHorizontalAlign::Left,
                vertical_align: RenderTextVerticalAlign::Top,
                layout_mode: RenderTextLayoutMode::SingleLine,
                overflow: RenderTextOverflow::Clip,
                ..Default::default()
            },
            font_source: None,
            direction: TextDirection::Auto,
            script: TextScript::Auto,
            language: None,
        }
    }

    #[test]
    fn generated_text_cache_key_changes_with_font_source() {
        let mut request = sample_text_request();
        let without_font = generated_text_cache_key(&request);
        request.font_source = Some("/tmp/custom-font-a.ttf".to_string());
        let with_font_a = generated_text_cache_key(&request);
        request.font_source = Some("/tmp/custom-font-b.ttf".to_string());
        let with_font_b = generated_text_cache_key(&request);

        assert_ne!(without_font, with_font_a);
        assert_ne!(with_font_a, with_font_b);
    }

    #[test]
    fn render_ops_applies_font_source_to_text_commands() {
        let mut commands = vec![
            FrameCommand::Text(sample_text_request()),
            FrameCommand::Clear {
                color: renderer_rgba(0, 0, 0, 255),
            },
        ];

        apply_font_source_to_commands(&mut commands, "/tmp/custom-font.ttf");

        match &commands[0] {
            FrameCommand::Text(request) => {
                assert_eq!(request.font_source.as_deref(), Some("/tmp/custom-font.ttf"));
            }
            _ => panic!("first command should remain text"),
        }
        assert!(matches!(commands[1], FrameCommand::Clear { .. }));
    }

    #[test]
    fn draw_text_request_respects_clip_rect_in_software_path() {
        let mut request = sample_text_request();
        request.rect = UiRect {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 40.0,
        };
        request.clip_rect = Some(UiRect {
            x: 60.0,
            y: 0.0,
            width: 140.0,
            height: 40.0,
        });
        request.text = "Custom font clipping".to_string();

        let width = 240usize;
        let height = 48usize;
        let mut rgba = vec![0u8; width * height * 4];
        draw_text_request(&mut rgba, width, height, &request);

        let left_alpha: usize = rgba
            .chunks_exact(4)
            .enumerate()
            .filter(|(idx, _)| idx % width < 60)
            .map(|(_, px)| px[3] as usize)
            .sum();
        let clipped_alpha: usize = rgba
            .chunks_exact(4)
            .enumerate()
            .filter(|(idx, _)| idx % width >= 60 && idx % width < 200)
            .map(|(_, px)| px[3] as usize)
            .sum();

        assert_eq!(left_alpha, 0);
        assert!(clipped_alpha > 0);
    }
}
