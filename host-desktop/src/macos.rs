use std::{
    cell::RefCell,
    env,
    ffi::CString,
    future::Future,
    path::Path,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
    time::{Duration, Instant},
};

use loadngo_gfx_metal::{
    measure_text_metrics as metal_measure_text_metrics, register_image_resource, MetalBackend,
};
use loadngo_host_core::{
    AssetIoBackend, DecodedImage, DesktopGraphicsBackend, DesktopPlatformBackend, FrameDemand,
    FrameTiming, HostFrame, InputSnapshot, RenderOp, RenderTextStyle, SurfaceInfo, TextMetrics,
    WindowDescriptor, WindowIconSet,
};
use loadngo_proactor::{CompletionKind, KqueuePort, Proactor, ProactorHandle, RunReport};
use loadngo_renderer::{FrameCommand, Renderer, RendererConfig};
use objc2::{
    class,
    encode::{Encode, Encoding},
    msg_send,
    rc::Retained,
    runtime::AnyObject,
};
use ui_core::{
    geometry::{Color as UiColor, Rect as UiRect},
    paint::PaintOp,
};

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

pub struct LoadngoPlatformHost;
pub struct LoadngoAssetIo;
pub struct LoadngoGraphicsHost;
pub struct LoadngoDesktopHost;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopRenderBackendKind {
    Macroquad,
    Metal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopRenderBackendStatus {
    pub requested: DesktopRenderBackendKind,
    pub last_used: DesktopRenderBackendKind,
    pub metal_initialized: bool,
    pub metal_surface_bound: bool,
    pub detail: String,
}

struct DesktopBackendRuntime {
    requested: DesktopRenderBackendKind,
    last_used: DesktopRenderBackendKind,
    metal: Option<MetalBackend>,
    pending_commands: Vec<FrameCommand>,
    pending_font_source: Option<String>,
    detail: String,
}

impl DesktopBackendRuntime {
    fn new() -> Self {
        Self {
            requested: requested_render_backend(),
            last_used: DesktopRenderBackendKind::Metal,
            metal: None,
            pending_commands: Vec::new(),
            pending_font_source: None,
            detail: "loadngo Metal backend waiting for the first frame".to_string(),
        }
    }

    fn ensure_metal_ready(&mut self) -> Result<&mut MetalBackend, String> {
        if self.metal.is_none() {
            let mut backend =
                MetalBackend::try_bind_system_default().map_err(|err| err.to_string())?;
            backend
                .try_bind_host_surface()
                .map_err(|err| err.to_string())?;
            self.detail = "loadngo Metal backend initialized and surface-bound".to_string();
            self.metal = Some(backend);
        }
        self.metal
            .as_mut()
            .ok_or_else(|| "Metal backend is unavailable".to_string())
    }

    fn status(&self) -> DesktopRenderBackendStatus {
        DesktopRenderBackendStatus {
            requested: self.requested,
            last_used: self.last_used,
            metal_initialized: self.metal.is_some(),
            metal_surface_bound: self
                .metal
                .as_ref()
                .map(|backend| backend.has_bound_surface())
                .unwrap_or(false),
            detail: self.detail.clone(),
        }
    }
}

#[derive(Clone, Copy)]
struct InputState {
    snapshot: InputSnapshot,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            snapshot: InputSnapshot {
                mouse_x: 0.0,
                mouse_y: 0.0,
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
            },
        }
    }
}

impl InputState {
    fn clear_transients(&mut self) {
        self.snapshot.mouse_wheel_y = 0.0;
        self.snapshot.mouse_pressed = false;
        self.snapshot.mouse_released = false;
        self.snapshot.escape_pressed = false;
        self.snapshot.space_pressed = false;
        self.snapshot.f3_pressed = false;
        self.snapshot.r_pressed = false;
        self.snapshot.up_pressed = false;
        self.snapshot.down_pressed = false;
        self.snapshot.touches = [None; 8];
    }

    fn apply_key_down(&mut self, key_code: u16) {
        match key_code {
            KEYCODE_ESCAPE => self.snapshot.escape_pressed = true,
            KEYCODE_SPACE => {
                if !self.snapshot.space_down {
                    self.snapshot.space_pressed = true;
                }
                self.snapshot.space_down = true;
            }
            KEYCODE_F3 => self.snapshot.f3_pressed = true,
            KEYCODE_R => self.snapshot.r_pressed = true,
            KEYCODE_UP => self.snapshot.up_pressed = true,
            KEYCODE_DOWN => self.snapshot.down_pressed = true,
            _ => {}
        }
    }

    fn apply_key_up(&mut self, key_code: u16) {
        if key_code == KEYCODE_SPACE {
            self.snapshot.space_down = false;
        }
    }
}

struct AppState {
    window: Retained<AnyObject>,
    view: Retained<AnyObject>,
    input: InputState,
    timing: FrameTiming,
    surface: SurfaceInfo,
    last_tick: Instant,
    frame_epoch: u64,
    event_epoch: u64,
    next_frame_wakers: Vec<Waker>,
    entry_future: Option<Pin<Box<dyn Future<Output = ()>>>>,
    should_close: bool,
}

thread_local! {
    static DESKTOP_BACKEND_RUNTIME: RefCell<Option<DesktopBackendRuntime>> = const { RefCell::new(None) };
    static APP_STATE: RefCell<Option<AppState>> = const { RefCell::new(None) };
    static MAC_PROACTOR: RefCell<Option<MacProactor>> = const { RefCell::new(None) };
    static TEXTURE_COUNTER: RefCell<u64> = const { RefCell::new(0) };
}

const NS_WINDOW_STYLE_MASK_TITLED: u64 = 1 << 0;
const NS_WINDOW_STYLE_MASK_CLOSABLE: u64 = 1 << 1;
const NS_WINDOW_STYLE_MASK_MINIATURIZABLE: u64 = 1 << 2;
const NS_WINDOW_STYLE_MASK_RESIZABLE: u64 = 1 << 3;
const NS_BACKING_STORE_BUFFERED: u64 = 2;
const NSEVENT_MASK_ANY: usize = usize::MAX;
const NSEVENT_TYPE_LEFT_MOUSE_DOWN: u64 = 1;
const NSEVENT_TYPE_LEFT_MOUSE_UP: u64 = 2;
const NSEVENT_TYPE_RIGHT_MOUSE_DOWN: u64 = 3;
const NSEVENT_TYPE_RIGHT_MOUSE_UP: u64 = 4;
const NSEVENT_TYPE_MOUSE_MOVED: u64 = 5;
const NSEVENT_TYPE_LEFT_MOUSE_DRAGGED: u64 = 6;
const NSEVENT_TYPE_RIGHT_MOUSE_DRAGGED: u64 = 7;
const NSEVENT_TYPE_KEY_DOWN: u64 = 10;
const NSEVENT_TYPE_KEY_UP: u64 = 11;
const NSEVENT_TYPE_SCROLL_WHEEL: u64 = 22;
const NSEVENT_TYPE_OTHER_MOUSE_DOWN: u64 = 25;
const NSEVENT_TYPE_OTHER_MOUSE_UP: u64 = 26;
const NSEVENT_TYPE_OTHER_MOUSE_DRAGGED: u64 = 27;
const WINDOW_RESIZE_CURSOR_MARGIN: f64 = 6.0;
const KEYCODE_ESCAPE: u16 = 53;
const KEYCODE_SPACE: u16 = 49;
const KEYCODE_R: u16 = 15;
const KEYCODE_F3: u16 = 99;
const KEYCODE_UP: u16 = 126;
const KEYCODE_DOWN: u16 = 125;
struct MacProactor {
    proactor: Proactor<KqueuePort>,
    handle: ProactorHandle<KqueuePort>,
}

impl MacProactor {
    fn new() -> Self {
        let proactor =
            Proactor::new(KqueuePort::new().expect("failed to create macOS kqueue port"));
        let handle = proactor.handle();
        Self { proactor, handle }
    }
}

#[derive(Clone)]
struct RuntimeWakeSignal {
    handle: ProactorHandle<KqueuePort>,
}

impl Wake for RuntimeWakeSignal {
    fn wake(self: Arc<Self>) {
        let _ = self.handle.wake();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let _ = self.handle.wake();
    }
}

impl DesktopPlatformBackend for LoadngoPlatformHost {
    fn launch<F>(window: WindowDescriptor, icon: Option<WindowIconSet>, entry: F)
    where
        F: Future<Output = ()> + 'static,
    {
        launch(window, icon, entry);
    }

    fn capture_frame() -> HostFrame {
        capture_frame()
    }

    fn next_frame(demand: FrameDemand) -> Pin<Box<dyn Future<Output = ()>>> {
        Box::pin(NextFrameFuture::new(demand))
    }

    fn simulate_mouse_with_touch(enabled: bool) {
        simulate_mouse_with_touch(enabled);
    }
}

impl AssetIoBackend for LoadngoAssetIo {
    fn load_bytes(path: &str) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>>>> {
        let path = path.to_string();
        Box::pin(async move {
            std::fs::read(&path).map_err(|err| format!("failed to read {path}: {err}"))
        })
    }

    fn load_text(path: &str) -> Pin<Box<dyn Future<Output = Result<String, String>>>> {
        let path = path.to_string();
        Box::pin(async move {
            std::fs::read_to_string(&path)
                .map_err(|err| format!("failed to read text {path}: {err}"))
        })
    }
}

impl DesktopGraphicsBackend for LoadngoGraphicsHost {
    type FontHandle = DesktopFont;
    type TextureHandle = DesktopTexture;

    fn load_font(path: &str) -> Pin<Box<dyn Future<Output = Result<Self::FontHandle, String>>>> {
        let path = path.to_string();
        Box::pin(async move {
            std::fs::metadata(&path).map_err(|err| format!("failed to stat font {path}: {err}"))?;
            Ok(DesktopFont::new(Some(path)))
        })
    }

    fn measure_text(
        text: &str,
        font: Option<&Self::FontHandle>,
        font_size: u16,
        font_scale: f32,
    ) -> TextMetrics {
        measure_text_metrics(text, font, font_size, font_scale)
    }

    fn render_ops(ops: &[RenderOp], font: Option<&Self::FontHandle>) {
        render_ops(ops, font);
    }

    fn upload_texture(image: &DecodedImage) -> Result<Self::TextureHandle, String> {
        upload_texture(image)
    }

    fn blit_texture(texture: &Self::TextureHandle, rect: UiRect, alpha: f32) {
        blit_texture(texture, rect, alpha);
    }
}

impl DesktopPlatformBackend for LoadngoDesktopHost {
    fn launch<F>(window: WindowDescriptor, icon: Option<WindowIconSet>, entry: F)
    where
        F: Future<Output = ()> + 'static,
    {
        LoadngoPlatformHost::launch(window, icon, entry);
    }

    fn capture_frame() -> HostFrame {
        LoadngoPlatformHost::capture_frame()
    }

    fn next_frame(demand: FrameDemand) -> Pin<Box<dyn Future<Output = ()>>> {
        LoadngoPlatformHost::next_frame(demand)
    }

    fn simulate_mouse_with_touch(enabled: bool) {
        LoadngoPlatformHost::simulate_mouse_with_touch(enabled);
    }
}

impl AssetIoBackend for LoadngoDesktopHost {
    fn load_bytes(path: &str) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>>>> {
        LoadngoAssetIo::load_bytes(path)
    }

    fn load_text(path: &str) -> Pin<Box<dyn Future<Output = Result<String, String>>>> {
        LoadngoAssetIo::load_text(path)
    }
}

impl DesktopGraphicsBackend for LoadngoDesktopHost {
    type FontHandle = DesktopFont;
    type TextureHandle = DesktopTexture;

    fn load_font(path: &str) -> Pin<Box<dyn Future<Output = Result<Self::FontHandle, String>>>> {
        LoadngoGraphicsHost::load_font(path)
    }

    fn measure_text(
        text: &str,
        font: Option<&Self::FontHandle>,
        font_size: u16,
        font_scale: f32,
    ) -> TextMetrics {
        LoadngoGraphicsHost::measure_text(text, font, font_size, font_scale)
    }

    fn render_ops(ops: &[RenderOp], font: Option<&Self::FontHandle>) {
        LoadngoGraphicsHost::render_ops(ops, font);
    }

    fn upload_texture(image: &DecodedImage) -> Result<Self::TextureHandle, String> {
        LoadngoGraphicsHost::upload_texture(image)
    }

    fn blit_texture(texture: &Self::TextureHandle, rect: UiRect, alpha: f32) {
        LoadngoGraphicsHost::blit_texture(texture, rect, alpha);
    }
}

fn requested_render_backend() -> DesktopRenderBackendKind {
    let _ = env::var("LOADNGO_DESKTOP_BACKEND");
    DesktopRenderBackendKind::Metal
}

fn with_desktop_backend_runtime<R>(f: impl FnOnce(&mut DesktopBackendRuntime) -> R) -> R {
    DESKTOP_BACKEND_RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        let runtime = runtime.get_or_insert_with(DesktopBackendRuntime::new);
        f(runtime)
    })
}

pub fn desktop_render_backend_status() -> DesktopRenderBackendStatus {
    with_desktop_backend_runtime(|runtime| runtime.status())
}

fn with_mac_proactor<R>(f: impl FnOnce(&MacProactor) -> R) -> R {
    MAC_PROACTOR.with(|proactor| {
        let proactor = proactor.borrow();
        let proactor = proactor
            .as_ref()
            .expect("macOS proactor is not initialized for loadngo host-desktop");
        f(proactor)
    })
}

fn runtime_waker() -> Waker {
    with_mac_proactor(|proactor| {
        Waker::from(Arc::new(RuntimeWakeSignal {
            handle: proactor.handle.clone(),
        }))
    })
}

pub fn launch(
    window: WindowDescriptor,
    icon: Option<WindowIconSet>,
    entry: impl Future<Output = ()> + 'static,
) {
    MAC_PROACTOR.with(|proactor| {
        *proactor.borrow_mut() = Some(MacProactor::new());
    });
    let (window_obj, view_obj, surface) = create_window(&window, icon.as_ref());
    APP_STATE.with(|state| {
        *state.borrow_mut() = Some(AppState {
            window: window_obj,
            view: view_obj,
            input: InputState::default(),
            timing: FrameTiming {
                delta_seconds: 1.0 / 60.0,
            },
            surface,
            last_tick: Instant::now(),
            frame_epoch: 0,
            event_epoch: 0,
            next_frame_wakers: Vec::new(),
            entry_future: Some(Box::pin(entry)),
            should_close: false,
        });
    });
    APP_STATE.with(|state| {
        if let Some(state) = state.borrow().as_ref() {
            with_desktop_backend_runtime(|runtime| {
                if runtime.requested == DesktopRenderBackendKind::Metal && runtime.metal.is_none() {
                    if let Ok(mut backend) = MetalBackend::try_bind_system_default() {
                        let view_ptr = (&*state.view) as *const AnyObject as *mut AnyObject;
                        if backend.try_bind_host_view_surface(view_ptr).is_ok() {
                            runtime.detail =
                                "loadngo Metal backend initialized and surface-bound".to_string();
                            runtime.metal = Some(backend);
                        }
                    }
                }
            });
        }
    });
    if poll_entry_future() {
        APP_STATE.with(|state| {
            if let Some(state) = state.borrow_mut().as_mut() {
                state.should_close = true;
            }
        });
    }
    run_event_loop();
    APP_STATE.with(|state| {
        state.borrow_mut().take();
    });
    MAC_PROACTOR.with(|proactor| {
        proactor.borrow_mut().take();
    });
}

pub fn capture_frame() -> HostFrame {
    APP_STATE.with(|state| {
        let mut state = state.borrow_mut();
        let state = state
            .as_mut()
            .expect("loadngo host-desktop app state is missing");
        let frame = HostFrame {
            timing: state.timing,
            surface: state.surface,
            input: state.input.snapshot,
        };
        state.input.clear_transients();
        frame
    })
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
                rect: ui_core::geometry::Rect {
                    x,
                    y: current_y,
                    width: metrics.width.max(1.0),
                    height: line_box_height,
                },
                text: line.clone(),
                style: RenderTextStyle {
                    color,
                    font_size,
                    horizontal_align: loadngo_host_core::RenderTextHorizontalAlign::Left,
                    vertical_align: loadngo_host_core::RenderTextVerticalAlign::Top,
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
            rect: ui_core::geometry::Rect {
                x,
                y,
                width: metrics.width.max(1.0),
                height: line_box_height,
            },
            text: text.to_string(),
            style: RenderTextStyle {
                color,
                font_size,
                horizontal_align: loadngo_host_core::RenderTextHorizontalAlign::Left,
                vertical_align: loadngo_host_core::RenderTextVerticalAlign::Top,
                layout_mode: loadngo_host_core::RenderTextLayoutMode::SingleLine,
                overflow: loadngo_host_core::RenderTextOverflow::Clip,
            },
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
        &[FrameCommand::Image(loadngo_renderer::ImageRequest {
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
        None => TEXTURE_COUNTER.with(|counter| {
            let mut counter = counter.borrow_mut();
            *counter += 1;
            format!("__loadngo_uploaded_texture_{}", *counter)
        }),
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

pub async fn next_frame(demand: FrameDemand) {
    NextFrameFuture::new(demand).await;
}

pub fn simulate_mouse_with_touch(_enabled: bool) {}

fn render_commands(commands: &[FrameCommand], font: Option<&DesktopFont>) {
    if commands.is_empty() {
        return;
    }
    with_desktop_backend_runtime(|runtime| {
        runtime.pending_commands.extend_from_slice(commands);
        if let Some(source) = font.and_then(DesktopFont::source_path) {
            runtime.pending_font_source = Some(source.to_string());
        }
        runtime.detail = "loadngo Metal backend queued the current frame commands".to_string();
    });
}

fn flush_selected_backend() {
    with_desktop_backend_runtime(|runtime| {
        if runtime.pending_commands.is_empty() {
            return;
        }

        let pending_commands = std::mem::take(&mut runtime.pending_commands);
        let pending_font_source = runtime.pending_font_source.take();
        let backend = match runtime.ensure_metal_ready() {
            Ok(backend) => backend,
            Err(err) => {
                runtime.detail = format!("Metal backend unavailable: {err}");
                return;
            }
        };
        backend.set_text_font_source(pending_font_source.as_deref());
        match Renderer::new(RendererConfig::default()).render(backend, &pending_commands) {
            Ok(()) => {
                runtime.last_used = DesktopRenderBackendKind::Metal;
                runtime.detail = "Metal backend rendered the queued frame".to_string();
            }
            Err(err) => {
                runtime.detail = format!("Metal backend render failed: {err}");
            }
        }
    });
}

fn font_size_and_scale(size: f32) -> (u16, f32) {
    let clamped = size.max(1.0);
    let font_size = clamped.round().min(u16::MAX as f32) as u16;
    let font_scale = (clamped / font_size as f32).max(0.01);
    (font_size, font_scale)
}

fn create_window(
    window: &WindowDescriptor,
    icon: Option<&WindowIconSet>,
) -> (Retained<AnyObject>, Retained<AnyObject>, SurfaceInfo) {
    unsafe {
        promote_process_to_foreground();
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, finishLaunching];
        if let Some(icon) = icon {
            if let Some(image) = ns_image_from_rgba(&icon.big_rgba8, 64, 64) {
                let _: () = msg_send![app, setApplicationIconImage: &*image];
            }
        }

        let width = window.width.unwrap_or(1280) as f64;
        let height = window.height.unwrap_or(720) as f64;
        let frame = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize { width, height },
        };
        let style_mask = NS_WINDOW_STYLE_MASK_TITLED
            | NS_WINDOW_STYLE_MASK_CLOSABLE
            | NS_WINDOW_STYLE_MASK_MINIATURIZABLE
            | NS_WINDOW_STYLE_MASK_RESIZABLE;
        let window_raw: *mut AnyObject = msg_send![class!(NSWindow), alloc];
        let window_raw: *mut AnyObject = msg_send![
            window_raw,
            initWithContentRect: frame,
            styleMask: style_mask,
            backing: NS_BACKING_STORE_BUFFERED,
            defer: false
        ];
        let window_obj = Retained::from_raw(window_raw)
            .expect("NSWindow allocation should succeed for loadngo host-desktop");
        let title = ns_string(&window.title);
        let _: () = msg_send![&*window_obj, setTitle: &*title];
        let _: () = msg_send![&*window_obj, setReleasedWhenClosed: false];
        let _: () = msg_send![&*window_obj, setAcceptsMouseMovedEvents: true];
        let _: () = msg_send![&*window_obj, center];
        let _: () =
            msg_send![&*window_obj, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];
        let _: () = msg_send![app, activateIgnoringOtherApps: true];

        let view_ptr: *mut AnyObject = msg_send![&*window_obj, contentView];
        let _: () = msg_send![&*window_obj, invalidateCursorRectsForView: view_ptr];
        let view_obj = Retained::retain(view_ptr)
            .expect("contentView should be retainable for loadngo host-desktop");
        let bounds: CGRect = msg_send![&*view_obj, bounds];
        (
            window_obj,
            view_obj,
            SurfaceInfo {
                width: bounds.size.width as f32,
                height: bounds.size.height as f32,
            },
        )
    }
}

fn promote_process_to_foreground() {
    unsafe {
        let mut psn = ProcessSerialNumber {
            high_long_of_psn: 0,
            low_long_of_psn: 0,
        };
        let status = GetCurrentProcess(&mut psn);
        if status == 0 {
            let _ = TransformProcessType(&mut psn, K_PROCESS_TRANSFORM_TO_FOREGROUND_APPLICATION);
            let _ = SetFrontProcess(&psn);
        }
    }
}

fn ns_image_from_rgba(rgba: &[u8], width: usize, height: usize) -> Option<Retained<AnyObject>> {
    if rgba.len() != width * height * 4 {
        return None;
    }
    unsafe {
        let rep_alloc: *mut AnyObject = msg_send![class!(NSBitmapImageRep), alloc];
        if rep_alloc.is_null() {
            return None;
        }
        let rep_raw: *mut AnyObject = msg_send![
            rep_alloc,
            initWithBitmapDataPlanes: std::ptr::null_mut::<*mut u8>(),
            pixelsWide: width,
            pixelsHigh: height,
            bitsPerSample: 8usize,
            samplesPerPixel: 4usize,
            hasAlpha: true,
            isPlanar: false,
            colorSpaceName: &*ns_string("NSCalibratedRGBColorSpace"),
            bitmapFormat: 0usize,
            bytesPerRow: width * 4,
            bitsPerPixel: 32usize
        ];
        let rep = Retained::from_raw(rep_raw)?;
        let bitmap_data: *mut u8 = msg_send![&*rep, bitmapData];
        if bitmap_data.is_null() {
            return None;
        }
        std::ptr::copy_nonoverlapping(rgba.as_ptr(), bitmap_data, rgba.len());

        let image_alloc: *mut AnyObject = msg_send![class!(NSImage), alloc];
        if image_alloc.is_null() {
            return None;
        }
        let image_raw: *mut AnyObject = msg_send![
            image_alloc,
            initWithSize: CGSize {
                width: width as f64,
                height: height as f64,
            }
        ];
        let image = Retained::from_raw(image_raw)?;
        let _: () = msg_send![&*image, addRepresentation: &*rep];
        Some(image)
    }
}

fn run_event_loop() {
    loop {
        let should_break = APP_STATE.with(|state| {
            state
                .borrow()
                .as_ref()
                .map(|s| s.should_close)
                .unwrap_or(true)
        });
        if should_break {
            break;
        }
        let timeout = with_mac_proactor(|proactor| {
            proactor
                .proactor
                .next_deadline()
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
        });
        pump_events_until(timeout);
        drain_proactor();
        if poll_entry_future() {
            APP_STATE.with(|state| {
                if let Some(state) = state.borrow_mut().as_mut() {
                    state.should_close = true;
                }
            });
        }
    }
}

fn pump_events_until(timeout: Option<Duration>) {
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let mode = ns_string("kCFRunLoopDefaultMode");
        let first_wait = ns_date_for_timeout(timeout);
        let event: *mut AnyObject = msg_send![
            app,
            nextEventMatchingMask: NSEVENT_MASK_ANY,
            untilDate: &*first_wait,
            inMode: &*mode,
            dequeue: true
        ];
        if !event.is_null() {
            handle_event(event);
            let _: () = msg_send![app, sendEvent: event];
            let distant_past: *mut AnyObject = msg_send![class!(NSDate), distantPast];
            loop {
                let event: *mut AnyObject = msg_send![
                    app,
                    nextEventMatchingMask: NSEVENT_MASK_ANY,
                    untilDate: distant_past,
                    inMode: &*mode,
                    dequeue: true
                ];
                if event.is_null() {
                    break;
                }
                handle_event(event);
                let _: () = msg_send![app, sendEvent: event];
            }
        }
        let _: () = msg_send![app, updateWindows];
    }
    APP_STATE.with(|state| {
        if let Some(state) = state.borrow_mut().as_mut() {
            let visible: bool = unsafe { msg_send![&*state.window, isVisible] };
            if !visible {
                state.should_close = true;
            }
            let bounds: CGRect = unsafe { msg_send![&*state.view, bounds] };
            state.surface = SurfaceInfo {
                width: bounds.size.width as f32,
                height: bounds.size.height as f32,
            };
            let _: () =
                unsafe { msg_send![&*state.window, invalidateCursorRectsForView: &*state.view] };
            update_window_cursor(&state.window);
        }
    });
}

fn drain_proactor() {
    with_mac_proactor(|proactor| loop {
        let report = proactor
            .proactor
            .run_ready()
            .expect("failed to drain macOS proactor");
        if !proactor_report_has_activity(report) {
            break;
        }
    });
}

fn handle_event(event: *mut AnyObject) {
    let event_type: u64 = unsafe { msg_send![event, type] };
    APP_STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(state) = state.as_mut() else {
            return;
        };
        match event_type {
            NSEVENT_TYPE_LEFT_MOUSE_DOWN
            | NSEVENT_TYPE_RIGHT_MOUSE_DOWN
            | NSEVENT_TYPE_OTHER_MOUSE_DOWN => {
                let (x, y) = event_point_in_view(&state.view, event);
                state.input.snapshot.mouse_x = x;
                state.input.snapshot.mouse_y = y;
                state.input.snapshot.mouse_pressed = true;
                state.input.snapshot.mouse_down = true;
            }
            NSEVENT_TYPE_LEFT_MOUSE_UP
            | NSEVENT_TYPE_RIGHT_MOUSE_UP
            | NSEVENT_TYPE_OTHER_MOUSE_UP => {
                let (x, y) = event_point_in_view(&state.view, event);
                state.input.snapshot.mouse_x = x;
                state.input.snapshot.mouse_y = y;
                state.input.snapshot.mouse_released = true;
                state.input.snapshot.mouse_down = false;
            }
            NSEVENT_TYPE_MOUSE_MOVED
            | NSEVENT_TYPE_LEFT_MOUSE_DRAGGED
            | NSEVENT_TYPE_RIGHT_MOUSE_DRAGGED
            | NSEVENT_TYPE_OTHER_MOUSE_DRAGGED => {
                let (x, y) = event_point_in_view(&state.view, event);
                state.input.snapshot.mouse_x = x;
                state.input.snapshot.mouse_y = y;
            }
            NSEVENT_TYPE_SCROLL_WHEEL => {
                let delta_y: f64 = unsafe { msg_send![event, scrollingDeltaY] };
                state.input.snapshot.mouse_wheel_y += delta_y as f32;
            }
            NSEVENT_TYPE_KEY_DOWN => {
                let key_code: u16 = unsafe { msg_send![event, keyCode] };
                state.input.apply_key_down(key_code);
            }
            NSEVENT_TYPE_KEY_UP => {
                let key_code: u16 = unsafe { msg_send![event, keyCode] };
                state.input.apply_key_up(key_code);
            }
            _ => {}
        }
        state.event_epoch = state.event_epoch.saturating_add(1);
        for waker in state.next_frame_wakers.drain(..) {
            waker.wake();
        }
    });
}

fn event_point_in_view(view: &Retained<AnyObject>, event: *mut AnyObject) -> (f32, f32) {
    unsafe {
        let point_in_window: CGPoint = msg_send![event, locationInWindow];
        let point_in_view: CGPoint = msg_send![&**view, convertPoint: point_in_window, fromView: std::ptr::null_mut::<AnyObject>()];
        let bounds: CGRect = msg_send![&**view, bounds];
        (
            point_in_view.x as f32,
            (bounds.size.height - point_in_view.y) as f32,
        )
    }
}

fn advance_frame_clock() {
    APP_STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(state) = state.as_mut() else {
            return;
        };
        let now = Instant::now();
        let dt = now.duration_since(state.last_tick).as_secs_f32();
        state.last_tick = now;
        state.timing = FrameTiming {
            delta_seconds: if dt > 0.0 { dt } else { 1.0 / 60.0 },
        };
        state.frame_epoch = state.frame_epoch.saturating_add(1);
        for waker in state.next_frame_wakers.drain(..) {
            waker.wake();
        }
    });
}

fn schedule_next_frame_tick(delay: Duration) {
    with_mac_proactor(|proactor| {
        let _ = proactor
            .handle
            .defer_for(delay, CompletionKind::Timer, 0, |_| advance_frame_clock());
    });
}

fn poll_entry_future() -> bool {
    let waker = runtime_waker();
    let mut cx = Context::from_waker(&waker);
    let mut future = APP_STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(state) = state.as_mut() else {
            return None;
        };
        state.entry_future.take()
    });

    let Some(mut future) = future.take() else {
        return true;
    };

    match future.as_mut().poll(&mut cx) {
        Poll::Ready(()) => true,
        Poll::Pending => {
            APP_STATE.with(|state| {
                if let Some(state) = state.borrow_mut().as_mut() {
                    state.entry_future = Some(future);
                }
            });
            false
        }
    }
}

struct NextFrameFuture {
    demand: FrameDemand,
    observed_event_epoch: u64,
    target_frame_epoch: u64,
    flushed: bool,
    scheduled: bool,
}

impl NextFrameFuture {
    fn new(demand: FrameDemand) -> Self {
        let (observed_event_epoch, target_frame_epoch) = APP_STATE.with(|state| {
            state
                .borrow()
                .as_ref()
                .map(|state| (state.event_epoch, state.frame_epoch.saturating_add(1)))
                .unwrap_or((0, 1))
        });
        Self {
            demand,
            observed_event_epoch,
            target_frame_epoch,
            flushed: false,
            scheduled: false,
        }
    }
}

impl Future for NextFrameFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if !self.flushed {
            flush_selected_backend();
            self.flushed = true;
        }
        let ready = APP_STATE.with(|state| {
            let mut state = state.borrow_mut();
            let Some(state) = state.as_mut() else {
                return true;
            };
            match self.demand {
                FrameDemand::Idle => {
                    if state.event_epoch > self.observed_event_epoch {
                        true
                    } else {
                        state.next_frame_wakers.push(cx.waker().clone());
                        false
                    }
                }
                FrameDemand::After(_) => {
                    if state.frame_epoch >= self.target_frame_epoch {
                        true
                    } else {
                        state.next_frame_wakers.push(cx.waker().clone());
                        false
                    }
                }
            }
        });
        if !ready {
            if !self.scheduled {
                if let FrameDemand::After(delay) = self.demand {
                    schedule_next_frame_tick(delay);
                    self.scheduled = true;
                }
            }
            return Poll::Pending;
        }
        Poll::Ready(())
    }
}

fn ns_string(value: &str) -> Retained<AnyObject> {
    let cstr = CString::new(value).expect("NSString source should not contain interior NUL");
    unsafe { msg_send![class!(NSString), stringWithUTF8String: cstr.as_ptr()] }
}

fn proactor_report_has_activity(report: RunReport) -> bool {
    report.dispatched_completions > 0
        || report.dispatched_deferred > 0
        || report.woke
        || report.stopped
}

fn ns_date_for_timeout(timeout: Option<Duration>) -> Retained<AnyObject> {
    unsafe {
        match timeout {
            Some(duration) if duration.is_zero() => {
                let date: *mut AnyObject = msg_send![class!(NSDate), distantPast];
                Retained::retain(date).expect("NSDate distantPast should be retainable")
            }
            Some(duration) => {
                let seconds = duration.as_secs_f64();
                let date: *mut AnyObject =
                    msg_send![class!(NSDate), dateWithTimeIntervalSinceNow: seconds];
                Retained::retain(date)
                    .expect("NSDate dateWithTimeIntervalSinceNow should be retainable")
            }
            None => {
                let date: *mut AnyObject = msg_send![class!(NSDate), distantFuture];
                Retained::retain(date).expect("NSDate distantFuture should be retainable")
            }
        }
    }
}

fn update_window_cursor(window: &Retained<AnyObject>) {
    unsafe {
        let mouse_location: CGPoint = msg_send![class!(NSEvent), mouseLocation];
        let frame: CGRect = msg_send![&**window, frame];
        let left = frame.origin.x;
        let right = frame.origin.x + frame.size.width;
        let bottom = frame.origin.y;
        let top = frame.origin.y + frame.size.height;

        let near_left = (mouse_location.x - left).abs() <= WINDOW_RESIZE_CURSOR_MARGIN;
        let near_right = (mouse_location.x - right).abs() <= WINDOW_RESIZE_CURSOR_MARGIN;
        let near_bottom = (mouse_location.y - bottom).abs() <= WINDOW_RESIZE_CURSOR_MARGIN;
        let near_top = (mouse_location.y - top).abs() <= WINDOW_RESIZE_CURSOR_MARGIN;
        let inside_x = mouse_location.x >= left && mouse_location.x <= right;
        let inside_y = mouse_location.y >= bottom && mouse_location.y <= top;

        let cursor: *mut AnyObject = if (near_left || near_right) && inside_y {
            msg_send![class!(NSCursor), resizeLeftRightCursor]
        } else if (near_top || near_bottom) && inside_x {
            msg_send![class!(NSCursor), resizeUpDownCursor]
        } else {
            msg_send![class!(NSCursor), arrowCursor]
        };
        if !cursor.is_null() {
            let _: () = msg_send![cursor, set];
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[repr(C)]
struct ProcessSerialNumber {
    high_long_of_psn: u32,
    low_long_of_psn: u32,
}

unsafe impl Encode for CGPoint {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}

unsafe impl Encode for CGSize {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}

unsafe impl Encode for CGRect {
    const ENCODING: Encoding = Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
}

const K_PROCESS_TRANSFORM_TO_FOREGROUND_APPLICATION: u32 = 1;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn GetCurrentProcess(psn: *mut ProcessSerialNumber) -> i32;
    fn TransformProcessType(psn: *mut ProcessSerialNumber, transform_state: u32) -> i32;
    fn SetFrontProcess(psn: *const ProcessSerialNumber) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_texture_registers_dimensions() {
        let image = DecodedImage::new(4, 2, vec![255; 4 * 2 * 4]);
        let texture = upload_texture(&image).expect("texture upload should succeed");
        assert_eq!(texture.width(), 4.0);
        assert_eq!(texture.height(), 2.0);
        assert!(texture
            .image_key()
            .starts_with("__loadngo_uploaded_texture_"));
    }
}
