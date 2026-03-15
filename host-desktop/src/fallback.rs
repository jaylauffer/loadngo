use std::{cell::RefCell, collections::HashMap, env, future::Future, path::Path};

use loadngo_gfx_metal::MetalBackend;
use loadngo_host_core::{
    decode_image_from_path, AssetIoBackend, DecodedImage, DesktopGraphicsBackend,
    DesktopPlatformBackend, FrameTiming, HostFrame, InputSnapshot, RenderOp, RenderTextStyle,
    SurfaceInfo, TextMetrics, TouchPhase, TouchPoint, WindowDescriptor, WindowIconSet,
};
use loadngo_renderer::{FrameCommand, Renderer, RendererConfig};
use macroquad::{file, prelude as mq};
use ui_core::{
    geometry::{Color as UiColor, Rect as UiRect},
    paint::PaintOp,
};

#[derive(Clone)]
pub struct DesktopFont {
    inner: mq::Font,
    source_path: Option<String>,
}

impl DesktopFont {
    fn new(inner: mq::Font, source_path: Option<String>) -> Self {
        Self { inner, source_path }
    }

    fn macroquad_font(&self) -> &mq::Font {
        &self.inner
    }

    pub fn source_path(&self) -> Option<&str> {
        self.source_path.as_deref()
    }
}

pub struct MacroquadPlatformHost;

pub struct MacroquadAssetIo;

pub struct MacroquadGraphicsHost;

pub struct MacroquadDesktopHost;

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
    metal: Option<MetalBackend>,
    pending_commands: Vec<FrameCommand>,
    pending_font_source: Option<String>,
    last_used: DesktopRenderBackendKind,
    detail: String,
}

thread_local! {
    static IMAGE_TEXTURES: RefCell<HashMap<String, DesktopTexture>> = RefCell::new(HashMap::new());
    static DESKTOP_BACKEND_RUNTIME: RefCell<Option<DesktopBackendRuntime>> = const { RefCell::new(None) };
}

#[derive(Clone)]
pub struct DesktopTexture {
    inner: mq::Texture2D,
    image_key: Option<String>,
}

impl DesktopTexture {
    fn new(inner: mq::Texture2D, image_key: Option<String>) -> Self {
        Self { inner, image_key }
    }

    pub fn width(&self) -> f32 {
        self.inner.width()
    }

    pub fn height(&self) -> f32 {
        self.inner.height()
    }

    pub fn image_key(&self) -> Option<&str> {
        self.image_key.as_deref()
    }
}

impl DesktopBackendRuntime {
    fn new() -> Self {
        let requested = requested_render_backend();
        Self {
            requested,
            metal: None,
            pending_commands: Vec::new(),
            pending_font_source: None,
            last_used: DesktopRenderBackendKind::Macroquad,
            detail: match requested {
                DesktopRenderBackendKind::Macroquad => "Macroquad backend selected".to_string(),
                DesktopRenderBackendKind::Metal => {
                    "Metal backend requested; waiting for a supported render pass".to_string()
                }
            },
        }
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

    fn ensure_metal_ready(&mut self) -> Result<&mut MetalBackend, String> {
        if self.metal.is_none() {
            let mut backend =
                MetalBackend::try_bind_system_default().map_err(|err| err.to_string())?;
            backend
                .try_bind_host_surface()
                .map_err(|err| err.to_string())?;
            self.detail = "Metal backend initialized and surface-bound".to_string();
            self.metal = Some(backend);
        }
        self.metal
            .as_mut()
            .ok_or_else(|| "Metal backend is unavailable".to_string())
    }
}

fn requested_render_backend() -> DesktopRenderBackendKind {
    match env::var("LOADNGO_DESKTOP_BACKEND")
        .ok()
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("metal") => DesktopRenderBackendKind::Metal,
        _ => DesktopRenderBackendKind::Macroquad,
    }
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

fn supports_metal_frame(commands: &[FrameCommand], font: Option<&DesktopFont>) -> bool {
    let _ = font;
    !commands.is_empty()
        && commands.iter().all(|command| {
            matches!(
                command,
                FrameCommand::Clear { .. }
                    | FrameCommand::FillRect { .. }
                    | FrameCommand::StrokeRect { .. }
                    | FrameCommand::Line { .. }
                    | FrameCommand::Circle { .. }
                    | FrameCommand::Text(..)
                    | FrameCommand::Image(..)
            )
        })
}

impl DesktopPlatformBackend for MacroquadPlatformHost {
    fn launch<F>(window: WindowDescriptor, icon: Option<WindowIconSet>, entry: F)
    where
        F: Future<Output = ()> + 'static,
    {
        launch(window, icon, entry);
    }

    fn capture_frame() -> HostFrame {
        capture_frame()
    }

    fn next_frame() -> std::pin::Pin<Box<dyn Future<Output = ()>>> {
        Box::pin(async { next_frame().await })
    }

    fn simulate_mouse_with_touch(enabled: bool) {
        simulate_mouse_with_touch(enabled);
    }
}

impl AssetIoBackend for MacroquadAssetIo {
    fn load_bytes(path: &str) -> std::pin::Pin<Box<dyn Future<Output = Result<Vec<u8>, String>>>> {
        let path = path.to_string();
        Box::pin(async move { load_bytes(&path).await })
    }

    fn load_text(path: &str) -> std::pin::Pin<Box<dyn Future<Output = Result<String, String>>>> {
        let path = path.to_string();
        Box::pin(async move { load_text(&path).await })
    }
}

impl DesktopGraphicsBackend for MacroquadGraphicsHost {
    type FontHandle = DesktopFont;
    type TextureHandle = DesktopTexture;

    fn load_font(
        path: &str,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<Self::FontHandle, String>>>> {
        let path = path.to_string();
        Box::pin(async move { load_font(&path).await })
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

impl DesktopPlatformBackend for MacroquadDesktopHost {
    fn launch<F>(window: WindowDescriptor, icon: Option<WindowIconSet>, entry: F)
    where
        F: Future<Output = ()> + 'static,
    {
        MacroquadPlatformHost::launch(window, icon, entry);
    }

    fn capture_frame() -> HostFrame {
        MacroquadPlatformHost::capture_frame()
    }

    fn next_frame() -> std::pin::Pin<Box<dyn Future<Output = ()>>> {
        MacroquadPlatformHost::next_frame()
    }

    fn simulate_mouse_with_touch(enabled: bool) {
        MacroquadPlatformHost::simulate_mouse_with_touch(enabled);
    }
}

impl AssetIoBackend for MacroquadDesktopHost {
    fn load_bytes(path: &str) -> std::pin::Pin<Box<dyn Future<Output = Result<Vec<u8>, String>>>> {
        MacroquadAssetIo::load_bytes(path)
    }

    fn load_text(path: &str) -> std::pin::Pin<Box<dyn Future<Output = Result<String, String>>>> {
        MacroquadAssetIo::load_text(path)
    }
}

impl DesktopGraphicsBackend for MacroquadDesktopHost {
    type FontHandle = DesktopFont;
    type TextureHandle = DesktopTexture;

    fn load_font(
        path: &str,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<Self::FontHandle, String>>>> {
        MacroquadGraphicsHost::load_font(path)
    }

    fn measure_text(
        text: &str,
        font: Option<&Self::FontHandle>,
        font_size: u16,
        font_scale: f32,
    ) -> TextMetrics {
        MacroquadGraphicsHost::measure_text(text, font, font_size, font_scale)
    }

    fn render_ops(ops: &[RenderOp], font: Option<&Self::FontHandle>) {
        MacroquadGraphicsHost::render_ops(ops, font);
    }

    fn upload_texture(image: &DecodedImage) -> Result<Self::TextureHandle, String> {
        MacroquadGraphicsHost::upload_texture(image)
    }

    fn blit_texture(texture: &Self::TextureHandle, rect: UiRect, alpha: f32) {
        MacroquadGraphicsHost::blit_texture(texture, rect, alpha);
    }
}

pub fn launch(
    window: WindowDescriptor,
    _icon: Option<WindowIconSet>,
    entry: impl Future<Output = ()> + 'static,
) {
    let mut conf = mq::Conf::default();
    conf.window_title = window.title;
    if let Some(width) = window.width {
        conf.window_width = width;
    }
    if let Some(height) = window.height {
        conf.window_height = height;
    }
    conf.high_dpi = window.high_dpi;
    if let Some(linux_wm_class) = window.linux_wm_class {
        conf.platform.linux_wm_class = linux_wm_class;
    }
    macroquad::Window::from_config(conf, entry);
}

pub fn capture_frame() -> HostFrame {
    let mouse_position = mq::mouse_position();
    let mouse_wheel = mq::mouse_wheel();
    let mut touches = [None; 8];
    for (slot, touch) in touches.iter_mut().zip(mq::touches().into_iter()) {
        *slot = Some(TouchPoint {
            id: touch.id,
            x: touch.position.x,
            y: touch.position.y,
            phase: match touch.phase {
                mq::TouchPhase::Started => TouchPhase::Started,
                mq::TouchPhase::Moved => TouchPhase::Moved,
                mq::TouchPhase::Stationary => TouchPhase::Stationary,
                mq::TouchPhase::Ended => TouchPhase::Ended,
                mq::TouchPhase::Cancelled => TouchPhase::Cancelled,
            },
        });
    }

    HostFrame {
        timing: FrameTiming {
            delta_seconds: mq::get_frame_time(),
        },
        surface: SurfaceInfo {
            width: mq::screen_width(),
            height: mq::screen_height(),
        },
        input: InputSnapshot {
            mouse_x: mouse_position.0,
            mouse_y: mouse_position.1,
            mouse_wheel_y: mouse_wheel.1,
            mouse_pressed: mq::is_mouse_button_pressed(mq::MouseButton::Left),
            mouse_down: mq::is_mouse_button_down(mq::MouseButton::Left),
            mouse_released: mq::is_mouse_button_released(mq::MouseButton::Left),
            touches,
            escape_pressed: mq::is_key_pressed(mq::KeyCode::Escape),
            space_pressed: mq::is_key_pressed(mq::KeyCode::Space),
            space_down: mq::is_key_down(mq::KeyCode::Space),
            f3_pressed: mq::is_key_pressed(mq::KeyCode::F3),
            r_pressed: mq::is_key_pressed(mq::KeyCode::R),
            up_pressed: mq::is_key_pressed(mq::KeyCode::Up),
            down_pressed: mq::is_key_pressed(mq::KeyCode::Down),
        },
    }
}

pub async fn load_bytes(path: &str) -> Result<Vec<u8>, String> {
    file::load_file(path).await.map_err(|err| err.to_string())
}

pub async fn load_text(path: &str) -> Result<String, String> {
    file::load_string(path).await.map_err(|err| err.to_string())
}

pub async fn load_font(path: &str) -> Result<DesktopFont, String> {
    let inner = mq::load_ttf_font(path)
        .await
        .map_err(|err| err.to_string())?;
    Ok(DesktopFont::new(inner, Some(path.to_string())))
}

pub fn measure_text_metrics(
    text: &str,
    font: Option<&DesktopFont>,
    font_size: u16,
    font_scale: f32,
) -> TextMetrics {
    #[cfg(target_os = "macos")]
    if let Ok(metrics) = loadngo_gfx_metal::measure_text_metrics(
        text,
        font.and_then(DesktopFont::source_path),
        font_size as f32 * font_scale,
    ) {
        return metrics;
    }

    let metrics = mq::measure_text(
        text,
        font.map(DesktopFont::macroquad_font),
        font_size,
        font_scale,
    );
    TextMetrics {
        width: metrics.width,
        height: metrics.height,
    }
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
            let dims = measure_text_metrics(&candidate, font, font_size, font_scale);
            if dims.width <= max_width {
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
    let line_height = font_size as f32 * font_scale;
    let mut current_y = y;
    let mut ops = Vec::new();
    for line in lines {
        if !line.is_empty() {
            ops.push(RenderOp::Text {
                rect: ui_core::geometry::Rect {
                    x: x as i32,
                    y: current_y as i32,
                    width: 0,
                    height: font_size as i32,
                },
                text: line.clone(),
                style: RenderTextStyle {
                    color,
                    font_size,
                    centered: false,
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
    render_commands(
        &[FrameCommand::Text(loadngo_renderer::TextRequest {
            rect: ui_core::geometry::Rect {
                x: x as i32,
                y: y as i32,
                width: 0,
                height: font_size as i32,
            },
            text: text.to_string(),
            style: RenderTextStyle {
                color,
                font_size,
                centered: false,
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
    if let Some(image_key) = texture.image_key() {
        render_commands(
            &[FrameCommand::Image(loadngo_renderer::ImageRequest {
                image_key: image_key.to_string(),
                rect,
                alpha,
            })],
            None,
        );
        return;
    }

    mq::draw_texture_ex(
        &texture.inner,
        rect.x as f32,
        rect.y as f32,
        mq::Color::new(1.0, 1.0, 1.0, alpha.clamp(0.0, 1.0)),
        mq::DrawTextureParams {
            dest_size: Some(mq::vec2(rect.width as f32, rect.height as f32)),
            ..Default::default()
        },
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
    if image.width > u16::MAX as u32 || image.height > u16::MAX as u32 {
        return Err(format!(
            "image is too large for GPU upload: {}x{}",
            image.width, image.height
        ));
    }
    let texture = mq::Texture2D::from_rgba8(image.width as u16, image.height as u16, &image.rgba8);
    texture.set_filter(mq::FilterMode::Linear);
    Ok(DesktopTexture::new(texture, image_key.map(str::to_string)))
}

pub fn draw_texture_fit(texture: &DesktopTexture, x: f32, y: f32, width: f32, height: f32) {
    let scale = (width / texture.width())
        .min(height / texture.height())
        .max(0.01);
    let draw_w = texture.width() * scale;
    let draw_h = texture.height() * scale;
    let draw_x = x + (width - draw_w) * 0.5;
    let draw_y = y + (height - draw_h) * 0.5;
    mq::draw_texture_ex(
        &texture.inner,
        draw_x,
        draw_y,
        mq::WHITE,
        mq::DrawTextureParams {
            dest_size: Some(mq::vec2(draw_w, draw_h)),
            ..Default::default()
        },
    );
}

pub fn draw_rectangle(x: f32, y: f32, w: f32, h: f32, color: UiColor) {
    mq::draw_rectangle(x, y, w, h, mq_color(color));
}

pub fn draw_rectangle_lines(x: f32, y: f32, w: f32, h: f32, thickness: f32, color: UiColor) {
    mq::draw_rectangle_lines(x, y, w, h, thickness, mq_color(color));
}

pub fn draw_text(text: &str, x: f32, y: f32, size: f32, color: UiColor) {
    let (font_size, font_scale) = font_size_and_scale(size);
    let _ = draw_text_with_font(text, x, y, None, font_size, font_scale, color);
}

pub fn measure_text(text: &str, _font: Option<()>, font_size: u16, font_scale: f32) -> TextMetrics {
    measure_text_metrics(text, None, font_size, font_scale)
}

pub async fn next_frame() {
    flush_selected_backend();
    mq::next_frame().await;
}

pub fn simulate_mouse_with_touch(enabled: bool) {
    mq::simulate_mouse_with_touch(enabled);
}

fn mq_color(color: UiColor) -> mq::Color {
    mq::Color::from_rgba(color.r, color.g, color.b, color.a)
}

fn try_render_commands_with_selected_backend(
    commands: &[FrameCommand],
    font: Option<&DesktopFont>,
) -> bool {
    with_desktop_backend_runtime(|runtime| {
        if runtime.requested != DesktopRenderBackendKind::Metal {
            runtime.last_used = DesktopRenderBackendKind::Macroquad;
            return false;
        }

        if !supports_metal_frame(commands, font) {
            runtime.last_used = DesktopRenderBackendKind::Macroquad;
            runtime.detail =
                "Metal backend requested, but this frame still needs unsupported draw commands"
                    .to_string();
            return false;
        }

        runtime.pending_commands.extend_from_slice(commands);
        if let Some(source) = font.and_then(DesktopFont::source_path) {
            runtime.pending_font_source = Some(source.to_string());
        }
        runtime.detail = "Metal backend queued the current frame commands".to_string();
        true
    })
}

fn flush_selected_backend() {
    with_desktop_backend_runtime(|runtime| {
        if runtime.requested != DesktopRenderBackendKind::Metal {
            runtime.pending_commands.clear();
            runtime.pending_font_source = None;
            runtime.last_used = DesktopRenderBackendKind::Macroquad;
            return;
        }

        if runtime.pending_commands.is_empty() {
            return;
        }

        let pending_commands = std::mem::take(&mut runtime.pending_commands);
        let pending_font_source = runtime.pending_font_source.take();
        let backend = match runtime.ensure_metal_ready() {
            Ok(backend) => backend,
            Err(err) => {
                runtime.last_used = DesktopRenderBackendKind::Macroquad;
                runtime.detail = format!("Metal backend requested but unavailable: {err}");
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
                runtime.last_used = DesktopRenderBackendKind::Macroquad;
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

fn draw_text_with_font(
    text: &str,
    x: f32,
    y: f32,
    font: Option<&DesktopFont>,
    font_size: u16,
    font_scale: f32,
    color: UiColor,
) -> TextMetrics {
    let metrics = measure_text_metrics(text, font, font_size, font_scale);
    mq::draw_text_ex(
        text,
        x,
        y,
        mq::TextParams {
            font: font.map(DesktopFont::macroquad_font),
            font_size,
            font_scale,
            color: mq_color(color),
            ..Default::default()
        },
    );
    metrics
}

fn render_commands(commands: &[FrameCommand], font: Option<&DesktopFont>) {
    if try_render_commands_with_selected_backend(commands, font) {
        return;
    }

    for command in commands {
        match command {
            FrameCommand::Clear { color } => mq::clear_background(mq_color(*color)),
            FrameCommand::FillRect { rect, color } => mq::draw_rectangle(
                rect.x as f32,
                rect.y as f32,
                rect.width as f32,
                rect.height as f32,
                mq_color(*color),
            ),
            FrameCommand::StrokeRect {
                rect,
                color,
                thickness,
            } => mq::draw_rectangle_lines(
                rect.x as f32,
                rect.y as f32,
                rect.width as f32,
                rect.height as f32,
                *thickness as f32,
                mq_color(*color),
            ),
            FrameCommand::Line {
                from,
                to,
                color,
                thickness,
            } => mq::draw_line(
                from.x as f32,
                from.y as f32,
                to.x as f32,
                to.y as f32,
                *thickness as f32,
                mq_color(*color),
            ),
            FrameCommand::Circle {
                center,
                radius,
                color,
            } => mq::draw_circle(
                center.x as f32,
                center.y as f32,
                *radius as f32,
                mq_color(*color),
            ),
            FrameCommand::Text(request) => {
                let metrics =
                    measure_text_metrics(&request.text, font, request.style.font_size, 1.0);
                let x = if request.style.centered {
                    request.rect.x as f32 + (request.rect.width as f32 - metrics.width) * 0.5
                } else {
                    request.rect.x as f32
                };
                let y = if request.style.centered {
                    request.rect.y as f32 + (request.rect.height as f32 + metrics.height) * 0.5
                        - 4.0
                } else {
                    request.rect.y as f32
                };
                let _ = draw_text_with_font(
                    &request.text,
                    x,
                    y,
                    font,
                    request.style.font_size,
                    1.0,
                    request.style.color,
                );
            }
            FrameCommand::Image(request) => {
                if let Some(texture) = cached_texture(&request.image_key) {
                    blit_texture(
                        &texture,
                        UiRect {
                            x: request.rect.x,
                            y: request.rect.y,
                            width: request.rect.width,
                            height: request.rect.height,
                        },
                        request.alpha,
                    );
                }
            }
        }
    }
}

fn cached_texture(image_key: &str) -> Option<DesktopTexture> {
    IMAGE_TEXTURES.with(|cache| {
        if let Some(texture) = cache.borrow().get(image_key) {
            return Some(texture.clone());
        }

        let decoded = decode_image_from_path(Path::new(image_key)).ok()?;
        let texture = upload_texture_with_image_key(Some(image_key), &decoded).ok()?;
        cache
            .borrow_mut()
            .insert(image_key.to_string(), texture.clone());
        Some(texture)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui_core::geometry::{Color, Rect};

    #[test]
    fn metal_subset_accepts_clear_and_rect_frames() {
        assert!(supports_metal_frame(
            &[FrameCommand::Clear {
                color: Color::rgba(0, 0, 0, 255),
            }],
            None
        ));
        assert!(supports_metal_frame(
            &[
                FrameCommand::Clear {
                    color: Color::rgba(0, 0, 0, 255),
                },
                FrameCommand::FillRect {
                    rect: Rect {
                        x: 0,
                        y: 0,
                        width: 10,
                        height: 10,
                    },
                    color: Color::rgba(255, 0, 0, 255),
                }
            ],
            None
        ));
        assert!(supports_metal_frame(
            &[FrameCommand::Image(loadngo_renderer::ImageRequest {
                image_key: "/tmp/test-image.png".to_string(),
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                },
                alpha: 1.0,
            })],
            None
        ));
        assert!(supports_metal_frame(
            &[FrameCommand::Line {
                from: ui_core::geometry::Point { x: 0, y: 0 },
                to: ui_core::geometry::Point { x: 10, y: 10 },
                color: Color::rgba(255, 255, 255, 255),
                thickness: 2,
            }],
            None
        ));
        assert!(supports_metal_frame(
            &[FrameCommand::Circle {
                center: ui_core::geometry::Point { x: 10, y: 10 },
                radius: 5,
                color: Color::rgba(255, 255, 255, 255),
            }],
            None
        ));
        assert!(supports_metal_frame(
            &[FrameCommand::Text(loadngo_renderer::TextRequest {
                text: "hello".to_string(),
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                },
                style: RenderTextStyle::default(),
                direction: loadngo_renderer::TextDirection::Auto,
                script: loadngo_renderer::TextScript::Auto,
                language: None,
            })],
            None
        ));
    }
}
