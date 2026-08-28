use std::{env, future::Future, path::Path, pin::Pin};

use loadngo_host_core::{
    AssetIoBackend, DecodedImage, DesktopGraphicsBackend, DesktopPlatformBackend, FrameDemand,
    FrameTiming, HostFrame, InputSnapshot, RenderOp, RenderTextStyle, SurfaceInfo, TextMetrics,
    WindowDescriptor, WindowIconSet,
};
use loadngo_renderer::{FrameCommand, Renderer, RendererConfig};
use ui_core::{
    geometry::{Color as UiColor, Rect as UiRect},
    paint::PaintOp,
};

#[derive(Clone, Default)]
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

#[derive(Clone, Default)]
pub struct DesktopTexture {
    image_key: Option<String>,
    width: f32,
    height: f32,
}

impl DesktopTexture {
    fn new(image_key: Option<String>, width: f32, height: f32) -> Self {
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

    pub fn image_key(&self) -> Option<&str> {
        self.image_key.as_deref()
    }
}

pub struct LoadngoPlaceholderPlatformHost;
pub struct LoadngoPlaceholderAssetIo;
pub struct LoadngoPlaceholderGraphicsHost;
pub struct LoadngoPlaceholderDesktopHost;

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

impl DesktopRenderBackendStatus {
    fn unavailable() -> Self {
        Self {
            requested: requested_render_backend(),
            last_used: DesktopRenderBackendKind::Unavailable,
            metal_initialized: false,
            metal_surface_bound: false,
            detail: unsupported_platform_detail(),
        }
    }
}

impl DesktopPlatformBackend for LoadngoPlaceholderPlatformHost {
    fn launch<F>(_window: WindowDescriptor, _icon: Option<WindowIconSet>, _entry: F)
    where
        F: Future<Output = ()> + 'static,
    {
        let message = unsupported_platform_detail();
        eprintln!("{message}");
        std::process::exit(1);
    }

    fn capture_frame() -> HostFrame {
        HostFrame {
            timing: FrameTiming { delta_seconds: 0.0 },
            surface: SurfaceInfo {
                width: 0.0,
                height: 0.0,
            },
            input: InputSnapshot {
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
            },
            foreground: true,
            insets: loadngo_host_core::SafeAreaInsets::default(),
        }
    }

    fn next_frame(_demand: FrameDemand) -> Pin<Box<dyn Future<Output = ()>>> {
        Box::pin(async {})
    }

    fn simulate_mouse_with_touch(_enabled: bool) {}
}

impl AssetIoBackend for LoadngoPlaceholderAssetIo {
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

impl DesktopGraphicsBackend for LoadngoPlaceholderGraphicsHost {
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
        _font: Option<&Self::FontHandle>,
        font_size: u16,
        font_scale: f32,
    ) -> TextMetrics {
        approximate_text_metrics(text, font_size, font_scale)
    }

    fn render_ops(_ops: &[RenderOp], _font: Option<&Self::FontHandle>) {}

    fn upload_texture(image: &DecodedImage) -> Result<Self::TextureHandle, String> {
        image.validate_rgba8()?;
        Ok(DesktopTexture::new(
            None,
            image.width as f32,
            image.height as f32,
        ))
    }

    fn blit_texture(_texture: &Self::TextureHandle, _rect: UiRect, _alpha: f32) {}
}

impl DesktopPlatformBackend for LoadngoPlaceholderDesktopHost {
    fn launch<F>(window: WindowDescriptor, icon: Option<WindowIconSet>, entry: F)
    where
        F: Future<Output = ()> + 'static,
    {
        LoadngoPlaceholderPlatformHost::launch(window, icon, entry);
    }

    fn capture_frame() -> HostFrame {
        LoadngoPlaceholderPlatformHost::capture_frame()
    }

    fn next_frame(demand: FrameDemand) -> Pin<Box<dyn Future<Output = ()>>> {
        LoadngoPlaceholderPlatformHost::next_frame(demand)
    }

    fn simulate_mouse_with_touch(enabled: bool) {
        LoadngoPlaceholderPlatformHost::simulate_mouse_with_touch(enabled);
    }
}

impl AssetIoBackend for LoadngoPlaceholderDesktopHost {
    fn load_bytes(path: &str) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>>>> {
        LoadngoPlaceholderAssetIo::load_bytes(path)
    }

    fn load_text(path: &str) -> Pin<Box<dyn Future<Output = Result<String, String>>>> {
        LoadngoPlaceholderAssetIo::load_text(path)
    }
}

impl DesktopGraphicsBackend for LoadngoPlaceholderDesktopHost {
    type FontHandle = DesktopFont;
    type TextureHandle = DesktopTexture;

    fn load_font(path: &str) -> Pin<Box<dyn Future<Output = Result<Self::FontHandle, String>>>> {
        LoadngoPlaceholderGraphicsHost::load_font(path)
    }

    fn measure_text(
        text: &str,
        font: Option<&Self::FontHandle>,
        font_size: u16,
        font_scale: f32,
    ) -> TextMetrics {
        LoadngoPlaceholderGraphicsHost::measure_text(text, font, font_size, font_scale)
    }

    fn render_ops(ops: &[RenderOp], font: Option<&Self::FontHandle>) {
        LoadngoPlaceholderGraphicsHost::render_ops(ops, font)
    }

    fn upload_texture(image: &DecodedImage) -> Result<Self::TextureHandle, String> {
        LoadngoPlaceholderGraphicsHost::upload_texture(image)
    }

    fn blit_texture(texture: &Self::TextureHandle, rect: UiRect, alpha: f32) {
        LoadngoPlaceholderGraphicsHost::blit_texture(texture, rect, alpha)
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
        _ => DesktopRenderBackendKind::Unavailable,
    }
}

fn unsupported_platform_detail() -> String {
    format!(
        "loadngo native desktop host is not implemented yet for target {}",
        std::env::consts::OS
    )
}

fn approximate_text_metrics(text: &str, font_size: u16, font_scale: f32) -> TextMetrics {
    let glyphs = text.chars().count() as f32;
    TextMetrics {
        width: glyphs * font_size as f32 * font_scale * 0.6,
        height: font_size as f32 * font_scale,
    }
}

pub fn desktop_render_backend_status() -> DesktopRenderBackendStatus {
    DesktopRenderBackendStatus::unavailable()
}

pub fn launch(
    window: WindowDescriptor,
    icon: Option<WindowIconSet>,
    entry: impl Future<Output = ()> + 'static,
) {
    LoadngoPlaceholderDesktopHost::launch(window, icon, entry);
}

pub fn capture_frame() -> HostFrame {
    LoadngoPlaceholderDesktopHost::capture_frame()
}

pub async fn load_bytes(path: &str) -> Result<Vec<u8>, String> {
    LoadngoPlaceholderAssetIo::load_bytes(path).await
}

pub async fn load_text(path: &str) -> Result<String, String> {
    LoadngoPlaceholderAssetIo::load_text(path).await
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
    LoadngoPlaceholderGraphicsHost::load_font(path).await
}

pub fn measure_text_metrics(
    text: &str,
    font: Option<&DesktopFont>,
    font_size: u16,
    font_scale: f32,
) -> TextMetrics {
    LoadngoPlaceholderGraphicsHost::measure_text(text, font, font_size, font_scale)
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
            let metrics = approximate_text_metrics(line, font_size, font_scale);
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

pub fn render_ops(ops: &[RenderOp], _font: Option<&DesktopFont>) {
    let _ = Renderer::new(RendererConfig::default()).encode_render_ops(ops);
}

pub fn render_widget_paint_ops(ops: &[PaintOp]) {
    let _ = Renderer::new(RendererConfig::default()).encode_paint_ops(ops);
}

pub fn clear(_color: UiColor) {
    let _ = [FrameCommand::Clear {
        color: UiColor::rgba(0, 0, 0, 0),
    }];
}

pub fn draw_plain_text(text: &str, _x: f32, _y: f32, size: f32, _color: UiColor) -> TextMetrics {
    let (font_size, font_scale) = font_size_and_scale(size);
    approximate_text_metrics(text, font_size, font_scale)
}

pub fn blit_texture(_texture: &DesktopTexture, _rect: UiRect, _alpha: f32) {}

pub fn upload_texture(image: &DecodedImage) -> Result<DesktopTexture, String> {
    LoadngoPlaceholderGraphicsHost::upload_texture(image)
}

pub fn upload_texture_with_image_key(
    image_key: Option<&str>,
    image: &DecodedImage,
) -> Result<DesktopTexture, String> {
    image.validate_rgba8()?;
    Ok(DesktopTexture::new(
        image_key.map(str::to_string),
        image.width as f32,
        image.height as f32,
    ))
}

pub fn draw_texture_fit(texture: &DesktopTexture, _x: f32, _y: f32, width: f32, height: f32) {
    let _ = (texture, width, height);
}

pub fn draw_rectangle(_x: f32, _y: f32, _w: f32, _h: f32, _color: UiColor) {}

pub fn draw_rectangle_lines(_x: f32, _y: f32, _w: f32, _h: f32, _thickness: f32, _color: UiColor) {}

pub fn draw_text(_text: &str, _x: f32, _y: f32, _size: f32, _color: UiColor) {}

pub fn measure_text(text: &str, _font: Option<()>, font_size: u16, font_scale: f32) -> TextMetrics {
    approximate_text_metrics(text, font_size, font_scale)
}

pub async fn next_frame(demand: FrameDemand) {
    LoadngoPlaceholderPlatformHost::next_frame(demand).await;
}

pub fn simulate_mouse_with_touch(_enabled: bool) {}

fn font_size_and_scale(size: f32) -> (u16, f32) {
    let clamped = size.max(1.0);
    let font_size = clamped.round().min(u16::MAX as f32) as u16;
    let font_scale = (clamped / font_size as f32).max(0.01);
    (font_size, font_scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_status_reports_unavailable_backend() {
        let status = desktop_render_backend_status();
        assert_eq!(status.last_used, DesktopRenderBackendKind::Unavailable);
        assert!(!status.metal_initialized);
        assert!(!status.metal_surface_bound);
    }
}
