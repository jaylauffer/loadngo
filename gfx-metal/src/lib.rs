use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        OnceLock,
    },
};

use loadngo_host_core::{decode_image_from_path, DecodedImage, TextMetrics};
use loadngo_renderer::{
    FrameCommand, FrameResourcePlan, GraphicsBackend, ImageResourceKey, Renderer, RendererConfig,
    RendererError, TextRequest,
};
use ui_core::geometry::Color;

thread_local! {
    static REGISTERED_IMAGES: RefCell<HashMap<String, DecodedImage>> = RefCell::new(HashMap::new());
}

fn trace_widgets_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("LOADNGO_TRACE_WIDGETS")
            .map(|value| {
                let value = value.trim().to_ascii_lowercase();
                value == "1" || value == "true" || value == "yes" || value == "on"
            })
            .unwrap_or(false)
    })
}

fn trace_widgets_log(message: impl AsRef<str>) {
    static LOG_COUNT: AtomicUsize = AtomicUsize::new(0);
    if !trace_widgets_enabled() {
        return;
    }
    let count = LOG_COUNT.fetch_add(1, Ordering::Relaxed);
    if count < 400 {
        eprintln!("[loadngo-trace] {}", message.as_ref());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetalBackendState {
    UnboundSurface,
    Headless,
    Ready,
    SurfaceBound,
}

pub struct MetalBackend {
    state: MetalBackendState,
    recorded_commands: Vec<FrameCommand>,
    frame_open: bool,
    text_font_source: Option<String>,
    #[cfg(target_os = "macos")]
    device: Option<macos::MetalDevice>,
    #[cfg(target_os = "macos")]
    command_queue: Option<macos::MetalCommandQueue>,
    #[cfg(target_os = "macos")]
    surface: Option<macos::MetalSurface>,
    #[cfg(target_os = "macos")]
    pipeline_state: Option<macos::MetalRenderPipelineState>,
    #[cfg(target_os = "macos")]
    textured_pipeline_state: Option<macos::MetalRenderPipelineState>,
    #[cfg(target_os = "macos")]
    sampler_state: Option<macos::MetalSamplerState>,
    #[cfg(target_os = "macos")]
    textures: HashMap<String, macos::MetalTexture>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ClearColor {
    red: f64,
    green: f64,
    blue: f64,
    alpha: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct SolidRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
}

#[derive(Debug, Clone, PartialEq)]
struct BlitImage {
    image_key: String,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    clip_rect: Option<ui_core::geometry::Rect>,
    alpha: f32,
    flip_vertical: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct GeneratedFrameImage {
    image: DecodedImage,
    placement: BlitImage,
}

#[derive(Debug, Clone, PartialEq)]
enum FrameVisual {
    SolidRect(SolidRect),
    RegisteredImage(BlitImage),
    GeneratedImage(GeneratedFrameImage),
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct RasterMetrics {
    width: f32,
    height: f32,
    baseline_from_top: f32,
}

pub fn measure_text_metrics(
    text: &str,
    font_source: Option<&str>,
    font_size: f32,
) -> Result<TextMetrics, RendererError> {
    #[cfg(target_os = "macos")]
    {
        return macos::measure_text_metrics(text, font_source, font_size);
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (text, font_source, font_size);
        Err(RendererError::Text(
            "native text measurement is unavailable on this platform".to_string(),
        ))
    }
}

pub fn register_image_resource(image_key: &str, image: &DecodedImage) {
    REGISTERED_IMAGES.with(|images| {
        images
            .borrow_mut()
            .insert(image_key.to_string(), image.clone());
    });
}

impl Default for MetalBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MetalBackend {
    pub fn new() -> Self {
        Self {
            state: MetalBackendState::UnboundSurface,
            recorded_commands: Vec::new(),
            frame_open: false,
            text_font_source: None,
            #[cfg(target_os = "macos")]
            device: None,
            #[cfg(target_os = "macos")]
            command_queue: None,
            #[cfg(target_os = "macos")]
            surface: None,
            #[cfg(target_os = "macos")]
            pipeline_state: None,
            #[cfg(target_os = "macos")]
            textured_pipeline_state: None,
            #[cfg(target_os = "macos")]
            sampler_state: None,
            #[cfg(target_os = "macos")]
            textures: HashMap::new(),
        }
    }

    pub fn new_headless() -> Self {
        Self {
            state: MetalBackendState::Headless,
            recorded_commands: Vec::new(),
            frame_open: false,
            text_font_source: None,
            #[cfg(target_os = "macos")]
            device: None,
            #[cfg(target_os = "macos")]
            command_queue: None,
            #[cfg(target_os = "macos")]
            surface: None,
            #[cfg(target_os = "macos")]
            pipeline_state: None,
            #[cfg(target_os = "macos")]
            textured_pipeline_state: None,
            #[cfg(target_os = "macos")]
            sampler_state: None,
            #[cfg(target_os = "macos")]
            textures: HashMap::new(),
        }
    }

    #[cfg(target_os = "macos")]
    pub fn try_bind_system_default() -> Result<Self, RendererError> {
        let device = macos::MetalDevice::system_default()?;
        let command_queue = device.new_command_queue()?;
        Ok(Self {
            state: MetalBackendState::Ready,
            recorded_commands: Vec::new(),
            frame_open: false,
            text_font_source: None,
            device: Some(device),
            command_queue: Some(command_queue),
            surface: None,
            pipeline_state: None,
            textured_pipeline_state: None,
            sampler_state: None,
            textures: HashMap::new(),
        })
    }

    #[cfg(not(target_os = "macos"))]
    pub fn try_bind_system_default() -> Result<Self, RendererError> {
        Err(RendererError::Backend(
            "Metal backend is only available on macOS".to_string(),
        ))
    }

    pub fn state(&self) -> MetalBackendState {
        self.state
    }

    #[cfg(target_os = "macos")]
    pub fn has_bound_device(&self) -> bool {
        self.device.is_some()
            && self
                .command_queue
                .as_ref()
                .map(|queue| {
                    let _ = queue.as_raw();
                    true
                })
                .unwrap_or(false)
    }

    #[cfg(target_os = "macos")]
    pub fn try_bind_host_surface(&mut self) -> Result<(), RendererError> {
        let device = self
            .device
            .as_ref()
            .ok_or_else(|| RendererError::Backend("Metal device is unavailable".to_string()))?;
        let surface = macos::MetalSurface::bind_to_host_window(device)?;
        self.surface = Some(surface);
        self.state = MetalBackendState::SurfaceBound;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    pub fn try_bind_host_view_surface(
        &mut self,
        view: *mut objc2::runtime::AnyObject,
    ) -> Result<(), RendererError> {
        let device = self
            .device
            .as_ref()
            .ok_or_else(|| RendererError::Backend("Metal device is unavailable".to_string()))?;
        let surface = macos::MetalSurface::bind_to_view(device, view)?;
        self.surface = Some(surface);
        self.state = MetalBackendState::SurfaceBound;
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    pub fn try_bind_host_surface(&mut self) -> Result<(), RendererError> {
        Err(RendererError::Backend(
            "Metal surfaces are only available on macOS".to_string(),
        ))
    }

    #[cfg(target_os = "macos")]
    pub fn has_bound_surface(&self) -> bool {
        self.surface
            .as_ref()
            .map(|surface| {
                let _ = surface.layer();
                true
            })
            .unwrap_or(false)
    }

    #[cfg(not(target_os = "macos"))]
    pub fn has_bound_surface(&self) -> bool {
        false
    }

    #[cfg(not(target_os = "macos"))]
    pub fn has_bound_device(&self) -> bool {
        false
    }

    pub fn take_recorded_commands(&mut self) -> Vec<FrameCommand> {
        std::mem::take(&mut self.recorded_commands)
    }

    pub fn set_text_font_source(&mut self, source: Option<&str>) {
        self.text_font_source = source.map(str::to_string);
    }

    fn frame_clear_color(&self) -> Option<ClearColor> {
        self.recorded_commands
            .iter()
            .rev()
            .find_map(|command| match command {
                FrameCommand::Clear { color } => Some(ClearColor {
                    red: color.r as f64 / 255.0,
                    green: color.g as f64 / 255.0,
                    blue: color.b as f64 / 255.0,
                    alpha: color.a as f64 / 255.0,
                }),
                _ => None,
            })
    }

    #[allow(dead_code)]
    fn frame_solid_rects(&self) -> Vec<SolidRect> {
        let mut rects = Vec::new();
        for command in &self.recorded_commands {
            match command {
                FrameCommand::FillRect { rect, color } => rects.push(SolidRect {
                    x: rect.x as f32,
                    y: rect.y as f32,
                    width: rect.width as f32,
                    height: rect.height as f32,
                    red: color.r as f32 / 255.0,
                    green: color.g as f32 / 255.0,
                    blue: color.b as f32 / 255.0,
                    alpha: color.a as f32 / 255.0,
                }),
                FrameCommand::StrokeRect {
                    rect,
                    color,
                    thickness,
                } => {
                    let t = (*thickness).max(1) as f32;
                    let rgba = (
                        color.r as f32 / 255.0,
                        color.g as f32 / 255.0,
                        color.b as f32 / 255.0,
                        color.a as f32 / 255.0,
                    );
                    let push =
                        |rects: &mut Vec<SolidRect>, x: f32, y: f32, width: f32, height: f32| {
                            if width > 0.0 && height > 0.0 {
                                rects.push(SolidRect {
                                    x,
                                    y,
                                    width,
                                    height,
                                    red: rgba.0,
                                    green: rgba.1,
                                    blue: rgba.2,
                                    alpha: rgba.3,
                                });
                            }
                        };
                    push(
                        &mut rects,
                        rect.x as f32,
                        rect.y as f32,
                        rect.width as f32,
                        t,
                    );
                    push(
                        &mut rects,
                        rect.x as f32,
                        rect.y as f32 + rect.height as f32 - t,
                        rect.width as f32,
                        t,
                    );
                    push(
                        &mut rects,
                        rect.x as f32,
                        rect.y as f32 + t,
                        t,
                        rect.height as f32 - 2.0 * t,
                    );
                    push(
                        &mut rects,
                        rect.x as f32 + rect.width as f32 - t,
                        rect.y as f32 + t,
                        t,
                        rect.height as f32 - 2.0 * t,
                    );
                }
                _ => {}
            }
        }
        rects
    }

    #[allow(dead_code)]
    fn frame_blit_images(&self) -> Vec<BlitImage> {
        self.recorded_commands
            .iter()
            .filter_map(|command| match command {
                FrameCommand::Image(request) => {
                    trace_widgets_log(format!(
                        "image request key='{}' rect=({}, {}, {}, {}) alpha={}",
                        request.image_key,
                        request.rect.x,
                        request.rect.y,
                        request.rect.width,
                        request.rect.height,
                        request.alpha
                    ));
                    Some(BlitImage {
                        image_key: request.image_key.clone(),
                        x: request.rect.x as f32,
                        y: request.rect.y as f32,
                        width: request.rect.width as f32,
                        height: request.rect.height as f32,
                        clip_rect: request.clip_rect,
                        alpha: request.alpha,
                        flip_vertical: false,
                    })
                }
                _ => None,
            })
            .collect()
    }

    #[allow(dead_code)]
    fn frame_generated_images(&self) -> Result<Vec<GeneratedFrameImage>, RendererError> {
        let mut images = Vec::new();
        for command in &self.recorded_commands {
            match command {
                FrameCommand::Text(request) => {
                    if request.text.is_empty() {
                        continue;
                    }
                    let raster = rasterize_text_request(request, self.text_font_source.as_deref())?;
                    let image_width = raster.image.width as f32;
                    let image_height = raster.image.height as f32;
                    trace_widgets_log(format!(
                        "text request='{}' h_align={:?} v_align={:?} mode={:?} overflow={:?} rect=({}, {}, {}, {}) logical=({}, {}) baseline={} image=({}, {}) content_top={} logical_top_display={} opaque_top_display={} opaque_height={} placement=({}, {})",
                        request.text,
                        request.style.horizontal_align,
                        request.style.vertical_align,
                        request.style.layout_mode,
                        request.style.overflow,
                        request.rect.x,
                        request.rect.y,
                        request.rect.width,
                        request.rect.height,
                        raster.metrics.width,
                        raster.metrics.height,
                        raster.metrics.baseline_from_top,
                        image_width,
                        image_height,
                        raster.content_top_in_image,
                        raster.logical_top_in_display,
                        raster.opaque_top_in_display,
                        raster.opaque_height,
                        raster.x,
                        raster.y,
                    ));
                    images.push(GeneratedFrameImage {
                        image: raster.image,
                        placement: BlitImage {
                            image_key: "__loadngo_text".to_string(),
                            x: raster.x,
                            y: raster.y,
                            width: image_width,
                            height: image_height,
                            clip_rect: Some(request.rect),
                            alpha: 1.0,
                            flip_vertical: true,
                        },
                    });
                }
                FrameCommand::Line {
                    from,
                    to,
                    color,
                    thickness,
                } => {
                    if let Some(image) = rasterize_line(*from, *to, *color, *thickness) {
                        images.push(image);
                    }
                }
                FrameCommand::Circle {
                    center,
                    radius,
                    color,
                } => {
                    if let Some(image) = rasterize_circle(*center, *radius, *color) {
                        images.push(image);
                    }
                }
                _ => {}
            }
        }
        Ok(images)
    }

    fn frame_visuals(&self) -> Result<Vec<FrameVisual>, RendererError> {
        let mut visuals = Vec::new();
        for command in &self.recorded_commands {
            match command {
                FrameCommand::FillRect { rect, color } => {
                    visuals.push(FrameVisual::SolidRect(SolidRect {
                        x: rect.x as f32,
                        y: rect.y as f32,
                        width: rect.width as f32,
                        height: rect.height as f32,
                        red: color.r as f32 / 255.0,
                        green: color.g as f32 / 255.0,
                        blue: color.b as f32 / 255.0,
                        alpha: color.a as f32 / 255.0,
                    }));
                }
                FrameCommand::StrokeRect {
                    rect,
                    color,
                    thickness,
                } => {
                    let t = (*thickness).max(1) as f32;
                    let rgba = (
                        color.r as f32 / 255.0,
                        color.g as f32 / 255.0,
                        color.b as f32 / 255.0,
                        color.a as f32 / 255.0,
                    );
                    let mut push = |x: f32, y: f32, width: f32, height: f32| {
                        if width > 0.0 && height > 0.0 {
                            visuals.push(FrameVisual::SolidRect(SolidRect {
                                x,
                                y,
                                width,
                                height,
                                red: rgba.0,
                                green: rgba.1,
                                blue: rgba.2,
                                alpha: rgba.3,
                            }));
                        }
                    };
                    push(rect.x as f32, rect.y as f32, rect.width as f32, t);
                    push(
                        rect.x as f32,
                        rect.y as f32 + rect.height as f32 - t,
                        rect.width as f32,
                        t,
                    );
                    push(
                        rect.x as f32,
                        rect.y as f32 + t,
                        t,
                        rect.height as f32 - 2.0 * t,
                    );
                    push(
                        rect.x as f32 + rect.width as f32 - t,
                        rect.y as f32 + t,
                        t,
                        rect.height as f32 - 2.0 * t,
                    );
                }
                FrameCommand::Image(request) => {
                    trace_widgets_log(format!(
                        "image request key='{}' rect=({}, {}, {}, {}) alpha={}",
                        request.image_key,
                        request.rect.x,
                        request.rect.y,
                        request.rect.width,
                        request.rect.height,
                        request.alpha
                    ));
                    visuals.push(FrameVisual::RegisteredImage(BlitImage {
                        image_key: request.image_key.clone(),
                        x: request.rect.x as f32,
                        y: request.rect.y as f32,
                        width: request.rect.width as f32,
                        height: request.rect.height as f32,
                        clip_rect: request.clip_rect,
                        alpha: request.alpha,
                        flip_vertical: false,
                    }));
                }
                FrameCommand::Text(request) => {
                    if request.text.is_empty() {
                        continue;
                    }
                    let raster = rasterize_text_request(request, self.text_font_source.as_deref())?;
                    let image_width = raster.image.width as f32;
                    let image_height = raster.image.height as f32;
                    trace_widgets_log(format!(
                        "text request='{}' h_align={:?} v_align={:?} mode={:?} overflow={:?} rect=({}, {}, {}, {}) logical=({}, {}) baseline={} image=({}, {}) content_top={} logical_top_display={} opaque_top_display={} opaque_height={} placement=({}, {})",
                        request.text,
                        request.style.horizontal_align,
                        request.style.vertical_align,
                        request.style.layout_mode,
                        request.style.overflow,
                        request.rect.x,
                        request.rect.y,
                        request.rect.width,
                        request.rect.height,
                        raster.metrics.width,
                        raster.metrics.height,
                        raster.metrics.baseline_from_top,
                        image_width,
                        image_height,
                        raster.content_top_in_image,
                        raster.logical_top_in_display,
                        raster.opaque_top_in_display,
                        raster.opaque_height,
                        raster.x,
                        raster.y,
                    ));
                    visuals.push(FrameVisual::GeneratedImage(GeneratedFrameImage {
                        image: raster.image,
                        placement: BlitImage {
                            image_key: "__loadngo_text".to_string(),
                            x: raster.x,
                            y: raster.y,
                            width: image_width,
                            height: image_height,
                            clip_rect: Some(request.rect),
                            alpha: 1.0,
                            flip_vertical: true,
                        },
                    }));
                }
                FrameCommand::Line {
                    from,
                    to,
                    color,
                    thickness,
                } => {
                    if let Some(image) = rasterize_line(*from, *to, *color, *thickness) {
                        visuals.push(FrameVisual::GeneratedImage(image));
                    }
                }
                FrameCommand::Circle {
                    center,
                    radius,
                    color,
                } => {
                    if let Some(image) = rasterize_circle(*center, *radius, *color) {
                        visuals.push(FrameVisual::GeneratedImage(image));
                    }
                }
                FrameCommand::Clear { .. } => {}
            }
        }
        Ok(visuals)
    }

    #[cfg(target_os = "macos")]
    fn ensure_image_resources(&mut self) -> Result<(), RendererError> {
        let renderer = Renderer::new(RendererConfig::default());
        let FrameResourcePlan { image_keys } =
            renderer.plan_frame_resources(&self.recorded_commands);
        for key in image_keys {
            self.ensure_texture(&key)?;
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn ensure_texture(&mut self, key: &ImageResourceKey) -> Result<(), RendererError> {
        if self.textures.contains_key(key.as_str()) {
            return Ok(());
        }
        let device = self
            .device
            .as_ref()
            .ok_or_else(|| RendererError::Backend("Metal device is unavailable".to_string()))?;
        let decoded = REGISTERED_IMAGES.with(|images| images.borrow().get(key.as_str()).cloned());
        let decoded = match decoded {
            Some(decoded) => decoded,
            None => decode_image_from_path(std::path::Path::new(key.as_str()))
                .map_err(RendererError::Backend)?,
        };
        trace_widgets_log(format!(
            "ensure_texture key='{}' decoded=({}, {})",
            key.as_str(),
            decoded.width,
            decoded.height
        ));
        let texture = macos::MetalTexture::from_decoded_image(device, key.as_str(), &decoded)?;
        self.textures.insert(key.as_str().to_string(), texture);
        Ok(())
    }

    fn ensure_bound(&self) -> Result<(), RendererError> {
        match self.state {
            MetalBackendState::Headless
            | MetalBackendState::Ready
            | MetalBackendState::SurfaceBound => Ok(()),
            MetalBackendState::UnboundSurface => Err(RendererError::Backend(
                "Metal backend is not bound to a drawable surface".to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct RasterizedText {
    image: DecodedImage,
    x: f32,
    y: f32,
    metrics: RasterMetrics,
    content_top_in_image: f32,
    logical_top_in_display: f32,
    opaque_top_in_display: f32,
    opaque_height: f32,
}

const TEXT_RASTER_ALPHA_TOP_MARGIN: u32 = 2;
const TEXT_RASTER_ALPHA_BOTTOM_MARGIN: u32 = 2;

fn rasterize_text_request(
    request: &TextRequest,
    font_source: Option<&str>,
) -> Result<RasterizedText, RendererError> {
    #[cfg(target_os = "macos")]
    {
        let mut request = request.clone();
        if matches!(
            request.style.layout_mode,
            loadngo_host_core::RenderTextLayoutMode::SingleLine
        ) && request.rect.width > 0.0
        {
            request.text = apply_single_line_overflow(
                &request.text,
                request.rect.width as f32,
                font_source,
                request.style.font_size as f32,
                &request.style.overflow,
            )?;
        }
        let mut raster = macos::rasterize_text(&request, font_source)?;
        let (crop_top, crop_bottom) =
            if let Some((opaque_top, opaque_bottom)) = opaque_alpha_bounds(&raster.image) {
                (
                    opaque_top.saturating_sub(TEXT_RASTER_ALPHA_TOP_MARGIN),
                    (opaque_bottom + 1 + TEXT_RASTER_ALPHA_BOTTOM_MARGIN).min(raster.image.height),
                )
            } else {
                let fallback_top = raster.content_top_in_image.floor().max(0.0) as u32;
                let fallback_bottom = (raster.content_top_in_image + raster.metrics.height)
                    .ceil()
                    .max(1.0)
                    .min(raster.image.height as f32) as u32;
                (fallback_top, fallback_bottom)
            };
        if crop_top < crop_bottom && (crop_top > 0 || crop_bottom < raster.image.height) {
            raster.image = crop_decoded_image_rows(&raster.image, crop_top, crop_bottom);
            raster.content_top_in_image = (raster.content_top_in_image - crop_top as f32).max(0.0);
        }
        let (opaque_top_in_image, opaque_bottom_in_image) = opaque_alpha_bounds(&raster.image)
            .unwrap_or_else(|| {
                let top = raster.content_top_in_image.max(0.0).floor() as u32;
                let bottom = (raster.content_top_in_image + raster.metrics.height)
                    .max(0.0)
                    .ceil()
                    .max(1.0) as u32
                    - 1;
                (top, bottom)
            });
        let opaque_height = (opaque_bottom_in_image + 1).saturating_sub(opaque_top_in_image) as f32;
        let logical_top_in_display = (raster.image.height as f32
            - (raster.content_top_in_image + raster.metrics.height))
            .max(0.0);
        let opaque_top_in_display =
            raster.image.height as f32 - 1.0 - opaque_bottom_in_image as f32;
        let text_x = match request.style.horizontal_align {
            loadngo_host_core::RenderTextHorizontalAlign::Left => request.rect.x as f32,
            loadngo_host_core::RenderTextHorizontalAlign::Center => {
                request.rect.x as f32
                    + (request.rect.width as f32 - raster.metrics.width).max(0.0) * 0.5
            }
            loadngo_host_core::RenderTextHorizontalAlign::Right => {
                request.rect.x as f32 + (request.rect.width as f32 - raster.metrics.width).max(0.0)
            }
        };
        let (visible_top, top_in_display) = match request.style.vertical_align {
            loadngo_host_core::RenderTextVerticalAlign::Top => {
                (request.rect.y as f32, opaque_top_in_display)
            }
            loadngo_host_core::RenderTextVerticalAlign::Middle => (
                request.rect.y as f32
                    + (request.rect.height as f32 - opaque_height).max(0.0) * 0.5,
                opaque_top_in_display,
            ),
            loadngo_host_core::RenderTextVerticalAlign::Bottom => (
                request.rect.y as f32 + (request.rect.height as f32 - opaque_height).max(0.0),
                opaque_top_in_display,
            ),
        };
        Ok(RasterizedText {
            image: raster.image,
            x: text_x,
            y: visible_top - top_in_display,
            metrics: raster.metrics,
            content_top_in_image: raster.content_top_in_image,
            logical_top_in_display,
            opaque_top_in_display,
            opaque_height,
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (request, font_source);
        Err(RendererError::Text(
            "native text rasterization is unavailable on this platform".to_string(),
        ))
    }
}

fn opaque_alpha_bounds(image: &DecodedImage) -> Option<(u32, u32)> {
    const MIN_ALPHA: u8 = 1;
    let width = image.width as usize;
    let height = image.height as usize;
    if width == 0 || height == 0 {
        return None;
    }
    let mut top = None;
    let mut bottom = None;
    for y in 0..height {
        let row_start = y * width * 4;
        let row_end = row_start + width * 4;
        let has_opaque = image.rgba8[row_start..row_end]
            .chunks_exact(4)
            .any(|px| px[3] >= MIN_ALPHA);
        if has_opaque {
            top.get_or_insert(y as u32);
            bottom = Some(y as u32);
        }
    }
    match (top, bottom) {
        (Some(top), Some(bottom)) => Some((top, bottom)),
        _ => None,
    }
}

fn crop_decoded_image_rows(image: &DecodedImage, start_row: u32, end_row: u32) -> DecodedImage {
    let start_row = start_row.min(image.height);
    let end_row = end_row.min(image.height).max(start_row);
    let new_height = end_row.saturating_sub(start_row).max(1);
    let row_bytes = image.width as usize * 4;
    let mut rgba8 = vec![0u8; row_bytes * new_height as usize];
    for row in 0..new_height as usize {
        let src_row = start_row as usize + row;
        let src_start = src_row * row_bytes;
        let src_end = src_start + row_bytes;
        let dst_start = row * row_bytes;
        let dst_end = dst_start + row_bytes;
        rgba8[dst_start..dst_end].copy_from_slice(&image.rgba8[src_start..src_end]);
    }
    DecodedImage::new(image.width, new_height, rgba8)
}

fn apply_single_line_overflow(
    text: &str,
    max_width: f32,
    font_source: Option<&str>,
    font_size: f32,
    overflow: &loadngo_host_core::RenderTextOverflow,
) -> Result<String, RendererError> {
    let normalized = text.replace('\n', " ");
    if normalized.is_empty() || max_width <= 0.0 {
        return Ok(String::new());
    }
    if measure_text_metrics(&normalized, font_source, font_size)?.width <= max_width {
        return Ok(normalized);
    }

    match overflow {
        loadngo_host_core::RenderTextOverflow::Clip => {
            fit_single_line_prefix(&normalized, "", max_width, font_source, font_size)
        }
        loadngo_host_core::RenderTextOverflow::EllipsisEnd => {
            fit_single_line_prefix(&normalized, "...", max_width, font_source, font_size)
        }
        loadngo_host_core::RenderTextOverflow::EllipsisMiddle => {
            fit_single_line_middle(&normalized, max_width, font_source, font_size)
        }
    }
}

fn fit_single_line_prefix(
    text: &str,
    suffix: &str,
    max_width: f32,
    font_source: Option<&str>,
    font_size: f32,
) -> Result<String, RendererError> {
    if !suffix.is_empty() && measure_text_metrics(suffix, font_source, font_size)?.width > max_width
    {
        return Ok(String::new());
    }

    let mut fitted = String::new();
    for ch in text.chars() {
        let mut candidate = fitted.clone();
        candidate.push(ch);
        let rendered = if suffix.is_empty() {
            candidate.clone()
        } else {
            format!("{candidate}{suffix}")
        };
        if measure_text_metrics(&rendered, font_source, font_size)?.width <= max_width {
            fitted = candidate;
        } else {
            break;
        }
    }

    if suffix.is_empty() || fitted.chars().count() == text.chars().count() {
        Ok(fitted)
    } else if fitted.is_empty() {
        Ok(suffix.to_string())
    } else {
        Ok(format!("{fitted}{suffix}"))
    }
}

fn fit_single_line_middle(
    text: &str,
    max_width: f32,
    font_source: Option<&str>,
    font_size: f32,
) -> Result<String, RendererError> {
    let ellipsis = "...";
    if measure_text_metrics(ellipsis, font_source, font_size)?.width > max_width {
        return Ok(String::new());
    }

    let chars: Vec<char> = text.chars().collect();
    let mut prefix = String::new();
    let mut suffix = String::new();
    let mut left = 0usize;
    let mut right = chars.len();

    while left < right {
        let try_prefix = format!("{prefix}{}", chars[left]);
        let candidate = format!("{try_prefix}{ellipsis}{suffix}");
        if measure_text_metrics(&candidate, font_source, font_size)?.width <= max_width {
            prefix = try_prefix;
            left += 1;
        } else {
            break;
        }

        if left >= right {
            break;
        }

        let try_suffix = format!("{}{}", chars[right - 1], suffix);
        let candidate = format!("{prefix}{ellipsis}{try_suffix}");
        if measure_text_metrics(&candidate, font_source, font_size)?.width <= max_width {
            suffix = try_suffix;
            right -= 1;
        } else {
            break;
        }
    }

    Ok(format!("{prefix}{ellipsis}{suffix}"))
}

fn rasterize_line(
    from: ui_core::geometry::Point,
    to: ui_core::geometry::Point,
    color: Color,
    thickness: i32,
) -> Option<GeneratedFrameImage> {
    let half = (thickness.max(1) as f32) * 0.5;
    let min_x = from.x.min(to.x) as f32 - half - 1.0;
    let min_y = from.y.min(to.y) as f32 - half - 1.0;
    let max_x = from.x.max(to.x) as f32 + half + 1.0;
    let max_y = from.y.max(to.y) as f32 + half + 1.0;
    let width = (max_x - min_x).ceil().max(1.0) as u32;
    let height = (max_y - min_y).ceil().max(1.0) as u32;
    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    let ax = from.x as f32 - min_x;
    let ay = from.y as f32 - min_y;
    let bx = to.x as f32 - min_x;
    let by = to.y as f32 - min_y;
    let radius = half.max(0.5);
    for y in 0..height {
        for x in 0..width {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            if distance_to_segment(px, py, ax, ay, bx, by) <= radius {
                write_rgba_pixel(&mut rgba, width, x, y, color);
            }
        }
    }
    Some(GeneratedFrameImage {
        image: DecodedImage::new(width, height, rgba),
        placement: BlitImage {
            image_key: "__loadngo_line".to_string(),
            x: min_x,
            y: min_y,
            width: width as f32,
            height: height as f32,
            clip_rect: None,
            alpha: 1.0,
            flip_vertical: false,
        },
    })
}

fn rasterize_circle(
    center: ui_core::geometry::Point,
    radius: i32,
    color: Color,
) -> Option<GeneratedFrameImage> {
    if radius <= 0 {
        return None;
    }
    let min_x = center.x - radius as f32 - 1.0;
    let min_y = center.y - radius as f32 - 1.0;
    let width = (radius * 2 + 2).max(1) as u32;
    let height = (radius * 2 + 2).max(1) as u32;
    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    let cx = radius as f32;
    let cy = radius as f32;
    let r = radius as f32;
    for y in 0..height {
        for x in 0..width {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            if dx * dx + dy * dy <= r * r {
                write_rgba_pixel(&mut rgba, width, x, y, color);
            }
        }
    }
    Some(GeneratedFrameImage {
        image: DecodedImage::new(width, height, rgba),
        placement: BlitImage {
            image_key: "__loadngo_circle".to_string(),
            x: min_x as f32,
            y: min_y as f32,
            width: width as f32,
            height: height as f32,
            clip_rect: None,
            alpha: 1.0,
            flip_vertical: false,
        },
    })
}

fn write_rgba_pixel(rgba: &mut [u8], width: u32, x: u32, y: u32, color: Color) {
    let idx = ((y * width + x) * 4) as usize;
    rgba[idx..idx + 4].copy_from_slice(&[color.r, color.g, color.b, color.a]);
}

fn distance_to_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let abx = bx - ax;
    let aby = by - ay;
    let length_sq = abx * abx + aby * aby;
    if length_sq <= f32::EPSILON {
        return ((px - ax).powi(2) + (py - ay).powi(2)).sqrt();
    }
    let t = (((px - ax) * abx + (py - ay) * aby) / length_sq).clamp(0.0, 1.0);
    let nearest_x = ax + t * abx;
    let nearest_y = ay + t * aby;
    ((px - nearest_x).powi(2) + (py - nearest_y).powi(2)).sqrt()
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{c_void, CString};

    use loadngo_host_core::{DecodedImage, TextMetrics};
    use loadngo_renderer::{RendererError, TextRequest};
    use objc2::encode::{Encode, Encoding};
    use objc2::{class, msg_send, rc::Retained, runtime::AnyObject};

    use crate::{BlitImage, ClearColor, RasterMetrics, RasterizedText, SolidRect};

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}
    #[link(name = "Metal", kind = "framework")]
    unsafe extern "C" {
        fn MTLCreateSystemDefaultDevice() -> *mut AnyObject;
    }
    #[link(name = "QuartzCore", kind = "framework")]
    unsafe extern "C" {}
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {}
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {}
    #[link(name = "CoreText", kind = "framework")]
    unsafe extern "C" {}

    type CFTypeRef = *const c_void;
    type CFStringRef = *const c_void;
    type CFDataRef = *const c_void;
    type CFDictionaryRef = *const c_void;
    type CFAttributedStringRef = *const c_void;
    type CTFontRef = *const c_void;
    type CTLineRef = *const c_void;
    type CGContextRef = *mut c_void;
    type CGColorSpaceRef = *const c_void;
    type CGDataProviderRef = *const c_void;
    type CGFontRef = *const c_void;

    #[repr(C)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    #[repr(C)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    #[repr(C)]
    struct CGAffineTransform {
        a: f64,
        b: f64,
        c: f64,
        d: f64,
        tx: f64,
        ty: f64,
    }

    unsafe extern "C" {
        static kCTFontAttributeName: CFStringRef;
        static kCTForegroundColorFromContextAttributeName: CFStringRef;
        static kCFBooleanTrue: CFTypeRef;

        fn CFStringCreateWithCString(
            alloc: *const c_void,
            c_str: *const i8,
            encoding: u32,
        ) -> CFStringRef;
        fn CFDataCreate(alloc: *const c_void, bytes: *const u8, length: isize) -> CFDataRef;
        fn CFDictionaryCreate(
            allocator: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFDictionaryRef;
        fn CFAttributedStringCreate(
            alloc: *const c_void,
            string: CFStringRef,
            attributes: CFDictionaryRef,
        ) -> CFAttributedStringRef;
        fn CFRelease(cf: CFTypeRef);

        fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
        fn CGColorSpaceRelease(space: CGColorSpaceRef);
        fn CGBitmapContextCreate(
            data: *mut c_void,
            width: usize,
            height: usize,
            bits_per_component: usize,
            bytes_per_row: usize,
            space: CGColorSpaceRef,
            bitmap_info: u32,
        ) -> CGContextRef;
        fn CGContextRelease(context: CGContextRef);
        fn CGContextTranslateCTM(context: CGContextRef, tx: f64, ty: f64);
        fn CGContextScaleCTM(context: CGContextRef, sx: f64, sy: f64);
        fn CGContextSetTextMatrix(context: CGContextRef, transform: CGAffineTransform);
        fn CGContextSetRGBFillColor(
            context: CGContextRef,
            red: f64,
            green: f64,
            blue: f64,
            alpha: f64,
        );
        fn CGContextSetTextPosition(context: CGContextRef, x: f64, y: f64);
        fn CGContextFillRect(context: CGContextRef, rect: CGRect);

        fn CGDataProviderCreateWithCFData(data: CFDataRef) -> CGDataProviderRef;
        fn CGDataProviderRelease(provider: CGDataProviderRef);
        fn CGFontCreateWithDataProvider(provider: CGDataProviderRef) -> CGFontRef;
        fn CGFontRelease(font: CGFontRef);

        fn CTFontCreateWithGraphicsFont(
            graphics_font: CGFontRef,
            size: f64,
            matrix: *const c_void,
            attributes: *const c_void,
        ) -> CTFontRef;
        fn CTFontCreateWithName(name: CFStringRef, size: f64, matrix: *const c_void) -> CTFontRef;
        fn CTLineCreateWithAttributedString(string: CFAttributedStringRef) -> CTLineRef;
        fn CTLineGetTypographicBounds(
            line: CTLineRef,
            ascent: *mut f64,
            descent: *mut f64,
            leading: *mut f64,
        ) -> f64;
        fn CTLineDraw(line: CTLineRef, context: CGContextRef);
    }

    pub struct MetalDevice {
        raw: Retained<AnyObject>,
    }

    impl MetalDevice {
        pub fn system_default() -> Result<Self, RendererError> {
            let raw = unsafe { Retained::from_raw(MTLCreateSystemDefaultDevice()) }
                .ok_or_else(|| RendererError::Backend("Metal device is unavailable".to_string()))?;
            Ok(Self { raw })
        }

        pub fn new_command_queue(&self) -> Result<MetalCommandQueue, RendererError> {
            let queue: Option<Retained<AnyObject>> =
                unsafe { msg_send![&*self.raw, newCommandQueue] };
            queue.map(|raw| MetalCommandQueue { raw }).ok_or_else(|| {
                RendererError::Backend("Metal command queue creation failed".to_string())
            })
        }

        pub fn as_raw(&self) -> &AnyObject {
            &self.raw
        }
    }

    pub struct MetalCommandQueue {
        raw: Retained<AnyObject>,
    }

    impl MetalCommandQueue {
        pub fn as_raw(&self) -> &AnyObject {
            &self.raw
        }
    }

    pub struct MetalSurface {
        view: Retained<AnyObject>,
        layer: Retained<AnyObject>,
    }

    impl MetalSurface {
        pub fn bind_to_host_window(device: &MetalDevice) -> Result<Self, RendererError> {
            let app: *mut AnyObject =
                unsafe { msg_send![class!(NSApplication), sharedApplication] };
            if app.is_null() {
                return Err(RendererError::Backend(
                    "NSApplication is unavailable".to_string(),
                ));
            }

            let mut window: *mut AnyObject = unsafe { msg_send![app, keyWindow] };
            if window.is_null() {
                window = unsafe { msg_send![app, mainWindow] };
            }
            if window.is_null() {
                return Err(RendererError::Backend(
                    "No host macOS window is available for Metal binding".to_string(),
                ));
            }

            let view: *mut AnyObject = unsafe { msg_send![window, contentView] };
            if view.is_null() {
                return Err(RendererError::Backend(
                    "Host macOS window has no content view".to_string(),
                ));
            }
            let view = unsafe { Retained::retain(view) }.ok_or_else(|| {
                RendererError::Backend("Failed to retain host macOS content view".to_string())
            })?;

            let layer: Retained<AnyObject> = unsafe { msg_send![class!(CAMetalLayer), new] };
            unsafe {
                let _: () = msg_send![&*layer, setDevice: device.as_raw()];
                let _: () = msg_send![&*layer, setFramebufferOnly: false];
                let _: () = msg_send![&*layer, setOpaque: true];
            }

            let scale: f64 = unsafe { msg_send![window, backingScaleFactor] };
            unsafe {
                let _: () = msg_send![&*layer, setContentsScale: scale];
                let _: () = msg_send![&*view, setWantsLayer: true];
                let _: () = msg_send![&*view, setLayer: &*layer];
            }

            let surface = Self { view, layer };
            surface.sync_drawable_size()?;
            Ok(surface)
        }

        pub fn bind_to_view(
            device: &MetalDevice,
            view: *mut AnyObject,
        ) -> Result<Self, RendererError> {
            if view.is_null() {
                return Err(RendererError::Backend(
                    "Host macOS content view is unavailable".to_string(),
                ));
            }
            let view = unsafe { Retained::retain(view) }.ok_or_else(|| {
                RendererError::Backend("Failed to retain host macOS content view".to_string())
            })?;

            let window: *mut AnyObject = unsafe { msg_send![&*view, window] };
            if window.is_null() {
                return Err(RendererError::Backend(
                    "Host macOS content view has no window".to_string(),
                ));
            }

            let layer: Retained<AnyObject> = unsafe { msg_send![class!(CAMetalLayer), new] };
            unsafe {
                let _: () = msg_send![&*layer, setDevice: device.as_raw()];
                let _: () = msg_send![&*layer, setFramebufferOnly: false];
                let _: () = msg_send![&*layer, setOpaque: true];
            }

            let scale: f64 = unsafe { msg_send![window, backingScaleFactor] };
            unsafe {
                let _: () = msg_send![&*layer, setContentsScale: scale];
                let _: () = msg_send![&*view, setWantsLayer: true];
                let _: () = msg_send![&*view, setLayer: &*layer];
            }

            let surface = Self { view, layer };
            surface.sync_drawable_size()?;
            Ok(surface)
        }

        pub fn layer(&self) -> &AnyObject {
            &self.layer
        }

        fn logical_size(&self) -> CGSize {
            let bounds: CGRect = unsafe { msg_send![&*self.view, bounds] };
            bounds.size
        }

        pub fn sync_drawable_size(&self) -> Result<(), RendererError> {
            let bounds: CGRect = unsafe { msg_send![&*self.view, bounds] };
            let window: *mut AnyObject = unsafe { msg_send![&*self.view, window] };
            let scale = if window.is_null() {
                1.0
            } else {
                unsafe { msg_send![window, backingScaleFactor] }
            };
            let drawable_size = CGSize {
                width: (bounds.size.width * scale).max(1.0),
                height: (bounds.size.height * scale).max(1.0),
            };
            unsafe {
                let _: () = msg_send![&*self.view, setWantsLayer: true];
                let _: () = msg_send![&*self.view, setLayer: &*self.layer];
                let _: () = msg_send![&*self.layer, setContentsScale: scale];
                let _: () = msg_send![&*self.layer, setFrame: bounds];
                let _: () = msg_send![&*self.layer, setDrawableSize: drawable_size];
            }
            Ok(())
        }
    }

    pub struct MetalRenderPipelineState {
        raw: Retained<AnyObject>,
    }

    impl MetalRenderPipelineState {
        pub fn new_solid(device: &MetalDevice) -> Result<Self, RendererError> {
            let library = shader_library(
                device,
                r#"
                #include <metal_stdlib>
                using namespace metal;

                struct VertexIn {
                    float2 position;
                };

                vertex float4 loadngo_vertex_main(
                    const device VertexIn* vertices [[buffer(0)]],
                    uint vid [[vertex_id]]
                ) {
                    return float4(vertices[vid].position, 0.0, 1.0);
                }

                fragment float4 loadngo_fragment_main(
                    const device float4* color [[buffer(0)]]
                ) {
                    return color[0];
                }
                "#,
            )?;
            let vertex_name = ns_string("loadngo_vertex_main")?;
            let fragment_name = ns_string("loadngo_fragment_main")?;
            Self::new_common(
                device,
                &library,
                &vertex_name,
                &fragment_name,
                MTL_PIXEL_FORMAT_BGRA8_UNORM,
                true,
            )
        }

        pub fn new_textured(device: &MetalDevice) -> Result<Self, RendererError> {
            let library = shader_library(
                device,
                r#"
                #include <metal_stdlib>
                using namespace metal;

                struct VertexIn {
                    float2 position;
                    float2 uv;
                };

                struct VertexOut {
                    float4 position [[position]];
                    float2 uv;
                };

                vertex VertexOut loadngo_texture_vertex_main(
                    const device VertexIn* vertices [[buffer(0)]],
                    uint vid [[vertex_id]]
                ) {
                    VertexOut out;
                    out.position = float4(vertices[vid].position, 0.0, 1.0);
                    out.uv = vertices[vid].uv;
                    return out;
                }

                fragment float4 loadngo_texture_fragment_main(
                    VertexOut in [[stage_in]],
                    texture2d<float> image [[texture(0)]],
                    sampler image_sampler [[sampler(0)]],
                    const device float* alpha [[buffer(0)]]
                ) {
                    float4 texel = image.sample(image_sampler, in.uv);
                    texel.a *= alpha[0];
                    return texel;
                }
                "#,
            )?;
            let vertex_name = ns_string("loadngo_texture_vertex_main")?;
            let fragment_name = ns_string("loadngo_texture_fragment_main")?;
            Self::new_common(
                device,
                &library,
                &vertex_name,
                &fragment_name,
                MTL_PIXEL_FORMAT_BGRA8_UNORM,
                true,
            )
        }

        fn new_common(
            device: &MetalDevice,
            library: &Retained<AnyObject>,
            vertex_name: &Retained<AnyObject>,
            fragment_name: &Retained<AnyObject>,
            pixel_format: u64,
            enable_blending: bool,
        ) -> Result<Self, RendererError> {
            let vertex_function: *mut AnyObject =
                unsafe { msg_send![&**library, newFunctionWithName: &**vertex_name] };
            let fragment_function: *mut AnyObject =
                unsafe { msg_send![&**library, newFunctionWithName: &**fragment_name] };
            if vertex_function.is_null() || fragment_function.is_null() {
                return Err(RendererError::Backend(
                    "Metal shader entry points were not found".to_string(),
                ));
            }

            let descriptor: Retained<AnyObject> =
                unsafe { msg_send![class!(MTLRenderPipelineDescriptor), new] };
            unsafe {
                let _: () = msg_send![&*descriptor, setVertexFunction: vertex_function];
                let _: () = msg_send![&*descriptor, setFragmentFunction: fragment_function];
            }
            let color_attachments: *mut AnyObject =
                unsafe { msg_send![&*descriptor, colorAttachments] };
            let color_attachment: *mut AnyObject =
                unsafe { msg_send![color_attachments, objectAtIndexedSubscript: 0usize] };
            if color_attachment.is_null() {
                return Err(RendererError::Backend(
                    "Metal pipeline color attachment 0 is unavailable".to_string(),
                ));
            }
            unsafe {
                let _: () = msg_send![color_attachment, setPixelFormat: pixel_format];
                let _: () = msg_send![color_attachment, setBlendingEnabled: enable_blending];
                if enable_blending {
                    let _: () =
                        msg_send![color_attachment, setRgbBlendOperation: MTL_BLEND_OPERATION_ADD];
                    let _: () = msg_send![color_attachment, setAlphaBlendOperation: MTL_BLEND_OPERATION_ADD];
                    let _: () = msg_send![
                        color_attachment,
                        setSourceRGBBlendFactor: MTL_BLEND_FACTOR_SOURCE_ALPHA
                    ];
                    let _: () = msg_send![
                        color_attachment,
                        setSourceAlphaBlendFactor: MTL_BLEND_FACTOR_ONE
                    ];
                    let _: () = msg_send![
                        color_attachment,
                        setDestinationRGBBlendFactor: MTL_BLEND_FACTOR_ONE_MINUS_SOURCE_ALPHA
                    ];
                    let _: () = msg_send![
                        color_attachment,
                        setDestinationAlphaBlendFactor: MTL_BLEND_FACTOR_ONE_MINUS_SOURCE_ALPHA
                    ];
                }
            }

            let mut error: *mut AnyObject = std::ptr::null_mut();
            let raw: Option<Retained<AnyObject>> = unsafe {
                msg_send![
                    device.as_raw(),
                    newRenderPipelineStateWithDescriptor: &*descriptor,
                    error: &mut error
                ]
            };
            raw.map(|raw| Self { raw })
                .ok_or_else(|| RendererError::Backend(library_error_message(error)))
        }

        pub fn as_raw(&self) -> &AnyObject {
            &self.raw
        }
    }

    pub struct MetalSamplerState {
        raw: Retained<AnyObject>,
    }

    impl MetalSamplerState {
        pub fn new_linear(device: &MetalDevice) -> Result<Self, RendererError> {
            let descriptor: Retained<AnyObject> =
                unsafe { msg_send![class!(MTLSamplerDescriptor), new] };
            unsafe {
                let _: () =
                    msg_send![&*descriptor, setMinFilter: MTL_SAMPLER_MIN_MAG_FILTER_LINEAR];
                let _: () =
                    msg_send![&*descriptor, setMagFilter: MTL_SAMPLER_MIN_MAG_FILTER_LINEAR];
            }
            let raw: *mut AnyObject =
                unsafe { msg_send![device.as_raw(), newSamplerStateWithDescriptor: &*descriptor] };
            unsafe { Retained::from_raw(raw) }
                .map(|raw| Self { raw })
                .ok_or_else(|| RendererError::Backend("Metal sampler creation failed".to_string()))
        }

        pub fn as_raw(&self) -> &AnyObject {
            &self.raw
        }
    }

    pub struct MetalTexture {
        raw: Retained<AnyObject>,
    }

    impl MetalTexture {
        pub fn from_decoded_image(
            device: &MetalDevice,
            image_key: &str,
            image: &DecodedImage,
        ) -> Result<Self, RendererError> {
            image.validate_rgba8().map_err(RendererError::Backend)?;
            let descriptor: *mut AnyObject = unsafe {
                msg_send![
                    class!(MTLTextureDescriptor),
                    texture2DDescriptorWithPixelFormat: MTL_PIXEL_FORMAT_RGBA8_UNORM,
                    width: image.width as u64,
                    height: image.height as u64,
                    mipmapped: false
                ]
            };
            if descriptor.is_null() {
                return Err(RendererError::Backend(
                    "Metal texture descriptor creation failed".to_string(),
                ));
            }

            let raw: *mut AnyObject =
                unsafe { msg_send![device.as_raw(), newTextureWithDescriptor: descriptor] };
            let raw = unsafe { Retained::from_raw(raw) }.ok_or_else(|| {
                RendererError::Backend("Metal texture creation failed".to_string())
            })?;

            let region = MTLRegion {
                origin: MTLOrigin { x: 0, y: 0, z: 0 },
                size: MTLSize {
                    width: image.width as u64,
                    height: image.height as u64,
                    depth: 1,
                },
            };
            unsafe {
                let _: () = msg_send![
                    &*raw,
                    replaceRegion: region,
                    mipmapLevel: 0u64,
                    withBytes: image.rgba8.as_ptr().cast::<c_void>(),
                    bytesPerRow: (image.width * 4) as u64
                ];
            }

            let _ = image_key;
            Ok(Self { raw })
        }

        pub fn as_raw(&self) -> &AnyObject {
            &self.raw
        }
    }

    pub fn measure_text_metrics(
        text: &str,
        font_source: Option<&str>,
        font_size: f32,
    ) -> Result<TextMetrics, RendererError> {
        let layout = layout_text(text, font_source, font_size.max(1.0))?;
        Ok(TextMetrics {
            width: layout.metrics.width,
            height: layout.metrics.height,
        })
    }

    pub fn rasterize_text(
        request: &TextRequest,
        font_source: Option<&str>,
    ) -> Result<RasterizedText, RendererError> {
        let color = request.style.color;
        let lines: Vec<String> = match request.style.layout_mode {
            loadngo_host_core::RenderTextLayoutMode::SingleLine => vec![request.text.clone()],
            loadngo_host_core::RenderTextLayoutMode::MultiLine => {
                let mut lines: Vec<String> = request.text.split('\n').map(str::to_string).collect();
                if lines.is_empty() {
                    lines.push(String::new());
                }
                lines
            }
        };
        let mut layouts = Vec::with_capacity(lines.len().max(1));
        for line in &lines {
            layouts.push(layout_text(
                line,
                font_source,
                request.style.font_size.max(1) as f32,
            )?);
        }
        let logical_width = layouts
            .iter()
            .map(|layout| layout.metrics.width)
            .fold(1.0f32, f32::max);
        let measured_line_height = layouts
            .iter()
            .map(|layout| layout.metrics.height)
            .fold(1.0f32, f32::max);
        let baseline_from_top = layouts
            .iter()
            .map(|layout| layout.metrics.baseline_from_top)
            .fold(1.0f32, f32::max);
        let line_box_height = ui_core::single_line_text_box_height(request.style.font_size);
        let line_step = ui_core::multiline_line_step(request.style.font_size);
        let logical_height = match request.style.layout_mode {
            loadngo_host_core::RenderTextLayoutMode::SingleLine => line_box_height.max(1.0),
            loadngo_host_core::RenderTextLayoutMode::MultiLine => {
                (line_box_height + line_step * lines.len().saturating_sub(1) as f32).max(1.0)
            }
        };
        // CoreText can draw a few pixels above the typographic ascent on macOS.
        // Keep extra headroom in the offscreen raster so editor/runtime labels do
        // not lose their top edge before alignment is applied.
        let pad_top = (request.style.font_size as f32 * 0.5).ceil().max(4.0) + 4.0;
        let pad_bottom = (request.style.font_size as f32 * 0.25).ceil().max(2.0) + 2.0;
        let width = logical_width.max(1.0).ceil() as usize;
        let height = (logical_height + pad_top + pad_bottom).max(1.0).ceil() as usize;
        let line_content_top = (line_box_height - measured_line_height).max(0.0) * 0.5;
        let mut rgba = vec![0u8; width * height * 4];
        let color_space = unsafe { CGColorSpaceCreateDeviceRGB() };
        if color_space.is_null() {
            return Err(RendererError::Text(
                "CoreGraphics RGB colorspace was unavailable".to_string(),
            ));
        }
        let bitmap_info = K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST | K_CG_BITMAP_BYTE_ORDER_32_BIG;
        let context = unsafe {
            CGBitmapContextCreate(
                rgba.as_mut_ptr().cast::<c_void>(),
                width,
                height,
                8,
                width * 4,
                color_space,
                bitmap_info,
            )
        };
        if context.is_null() {
            unsafe { CGColorSpaceRelease(color_space) };
            return Err(RendererError::Text(
                "CoreGraphics bitmap context creation failed".to_string(),
            ));
        }

        unsafe {
            CGContextSetRGBFillColor(context, 0.0, 0.0, 0.0, 0.0);
            CGContextFillRect(
                context,
                CGRect {
                    origin: CGPoint { x: 0.0, y: 0.0 },
                    size: CGSize {
                        width: width as f64,
                        height: height as f64,
                    },
                },
            );
            CGContextSetTextMatrix(
                context,
                CGAffineTransform {
                    a: 1.0,
                    b: 0.0,
                    c: 0.0,
                    d: 1.0,
                    tx: 0.0,
                    ty: 0.0,
                },
            );
            CGContextTranslateCTM(context, 0.0, height as f64);
            CGContextScaleCTM(context, 1.0, -1.0);
            CGContextSetRGBFillColor(
                context,
                color.r as f64 / 255.0,
                color.g as f64 / 255.0,
                color.b as f64 / 255.0,
                color.a as f64 / 255.0,
            );
            for (index, layout) in layouts.iter().enumerate() {
                let line_index = match request.style.layout_mode {
                    loadngo_host_core::RenderTextLayoutMode::SingleLine => 0,
                    loadngo_host_core::RenderTextLayoutMode::MultiLine => {
                        lines.len().saturating_sub(1).saturating_sub(index)
                    }
                };
                CGContextSetTextPosition(
                    context,
                    0.0,
                    (pad_top + line_content_top + baseline_from_top + line_step * line_index as f32)
                        as f64,
                );
                CTLineDraw(layout.line, context);
            }
            CGContextRelease(context);
            CGColorSpaceRelease(color_space);
        }

        for layout in layouts {
            release_cf(layout.line);
            release_cf(layout.attributed_string);
            release_cf(layout.font);
            release_cf(layout.string);
        }

        Ok(RasterizedText {
            image: DecodedImage::new(width as u32, height as u32, rgba),
            x: 0.0,
            y: 0.0,
            metrics: RasterMetrics {
                width: logical_width,
                height: logical_height,
                baseline_from_top,
            },
            content_top_in_image: pad_top,
            logical_top_in_display: pad_bottom,
            opaque_top_in_display: 0.0,
            opaque_height: logical_height,
        })
    }

    struct TextLayout {
        font: CTFontRef,
        string: CFStringRef,
        attributed_string: CFAttributedStringRef,
        line: CTLineRef,
        metrics: RasterMetrics,
    }

    fn layout_text(
        text: &str,
        font_source: Option<&str>,
        font_size: f32,
    ) -> Result<TextLayout, RendererError> {
        let string = cf_string(text)?;
        let font = create_font(font_source, font_size)?;
        let (keys, values) = unsafe {
            (
                [
                    kCTFontAttributeName,
                    kCTForegroundColorFromContextAttributeName,
                ],
                [font.cast::<c_void>(), kCFBooleanTrue],
            )
        };
        let attributes = unsafe {
            CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr().cast::<*const c_void>(),
                values.as_ptr(),
                keys.len() as isize,
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if attributes.is_null() {
            release_cf(font);
            release_cf(string);
            return Err(RendererError::Text(
                "CoreFoundation attribute dictionary creation failed".to_string(),
            ));
        }
        let attributed = unsafe { CFAttributedStringCreate(std::ptr::null(), string, attributes) };
        if attributed.is_null() {
            release_cf(attributes);
            release_cf(font);
            release_cf(string);
            return Err(RendererError::Text(
                "CoreFoundation attributed string creation failed".to_string(),
            ));
        }
        release_cf(attributes);
        let line = unsafe { CTLineCreateWithAttributedString(attributed) };
        if line.is_null() {
            release_cf(attributed);
            release_cf(font);
            release_cf(string);
            return Err(RendererError::Text(
                "CoreText line creation failed".to_string(),
            ));
        }

        let mut ascent = 0.0;
        let mut descent = 0.0;
        let mut leading = 0.0;
        let width =
            unsafe { CTLineGetTypographicBounds(line, &mut ascent, &mut descent, &mut leading) };
        let metrics = RasterMetrics {
            width: width.max(0.0).ceil() as f32,
            height: (ascent + descent + leading).max(1.0).ceil() as f32,
            baseline_from_top: ascent.max(0.0).ceil() as f32,
        };
        Ok(TextLayout {
            font,
            string,
            attributed_string: attributed,
            line,
            metrics,
        })
    }

    fn create_font(font_source: Option<&str>, font_size: f32) -> Result<CTFontRef, RendererError> {
        if let Some(path) = font_source {
            let bytes = std::fs::read(path).map_err(|err| {
                RendererError::Text(format!("failed to read font source {path}: {err}"))
            })?;
            let data =
                unsafe { CFDataCreate(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize) };
            if data.is_null() {
                return Err(RendererError::Text(
                    "CoreFoundation font data creation failed".to_string(),
                ));
            }
            let provider = unsafe { CGDataProviderCreateWithCFData(data) };
            if provider.is_null() {
                release_cf(data);
                return Err(RendererError::Text(
                    "CoreGraphics data provider creation failed".to_string(),
                ));
            }
            let font = unsafe { CGFontCreateWithDataProvider(provider) };
            unsafe {
                CGDataProviderRelease(provider);
                release_cf(data);
            }
            if font.is_null() {
                return Err(RendererError::Text(
                    "CoreGraphics font creation failed".to_string(),
                ));
            }
            let ct_font = unsafe {
                CTFontCreateWithGraphicsFont(
                    font,
                    font_size as f64,
                    std::ptr::null(),
                    std::ptr::null(),
                )
            };
            unsafe { CGFontRelease(font) };
            if ct_font.is_null() {
                return Err(RendererError::Text(
                    "CoreText font creation from graphics font failed".to_string(),
                ));
            }
            return Ok(ct_font);
        }

        let family = cf_string("Helvetica")?;
        let font = unsafe { CTFontCreateWithName(family, font_size as f64, std::ptr::null()) };
        release_cf(family);
        if font.is_null() {
            return Err(RendererError::Text(
                "CoreText system font creation failed".to_string(),
            ));
        }
        Ok(font)
    }

    fn cf_string(value: &str) -> Result<CFStringRef, RendererError> {
        let cstr = CString::new(value).map_err(|err| {
            RendererError::Text(format!("failed to build CoreFoundation string: {err}"))
        })?;
        let string = unsafe {
            CFStringCreateWithCString(std::ptr::null(), cstr.as_ptr(), K_CF_STRING_ENCODING_UTF8)
        };
        if string.is_null() {
            return Err(RendererError::Text(
                "CoreFoundation string creation failed".to_string(),
            ));
        }
        Ok(string)
    }

    fn release_cf(value: CFTypeRef) {
        if !value.is_null() {
            unsafe { CFRelease(value) };
        }
    }

    #[repr(C)]
    struct MTLClearColor {
        red: f64,
        green: f64,
        blue: f64,
        alpha: f64,
    }

    unsafe impl Encode for MTLClearColor {
        const ENCODING: Encoding = Encoding::Struct(
            "?",
            &[f64::ENCODING, f64::ENCODING, f64::ENCODING, f64::ENCODING],
        );
    }

    const MTL_LOAD_ACTION_CLEAR: u64 = 2;
    const MTL_STORE_ACTION_STORE: u64 = 1;
    const MTL_PIXEL_FORMAT_BGRA8_UNORM: u64 = 80;
    const MTL_PIXEL_FORMAT_RGBA8_UNORM: u64 = 70;
    const MTL_PRIMITIVE_TYPE_TRIANGLE: u64 = 3;
    const MTL_SAMPLER_MIN_MAG_FILTER_LINEAR: u64 = 1;
    const MTL_BLEND_OPERATION_ADD: u64 = 0;
    const MTL_BLEND_FACTOR_ONE: u64 = 1;
    const MTL_BLEND_FACTOR_SOURCE_ALPHA: u64 = 4;
    const MTL_BLEND_FACTOR_ONE_MINUS_SOURCE_ALPHA: u64 = 5;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST: u32 = 1;
    const K_CG_BITMAP_BYTE_ORDER_32_BIG: u32 = 4 << 12;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MetalVertex {
        x: f32,
        y: f32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MetalTexturedVertex {
        x: f32,
        y: f32,
        u: f32,
        v: f32,
    }

    #[repr(C)]
    struct MTLOrigin {
        x: u64,
        y: u64,
        z: u64,
    }

    unsafe impl Encode for MTLOrigin {
        const ENCODING: Encoding =
            Encoding::Struct("?", &[u64::ENCODING, u64::ENCODING, u64::ENCODING]);
    }

    #[repr(C)]
    struct MTLSize {
        width: u64,
        height: u64,
        depth: u64,
    }

    unsafe impl Encode for MTLSize {
        const ENCODING: Encoding =
            Encoding::Struct("?", &[u64::ENCODING, u64::ENCODING, u64::ENCODING]);
    }

    #[repr(C)]
    struct MTLRegion {
        origin: MTLOrigin,
        size: MTLSize,
    }

    unsafe impl Encode for MTLRegion {
        const ENCODING: Encoding = Encoding::Struct("?", &[MTLOrigin::ENCODING, MTLSize::ENCODING]);
    }

    unsafe impl Encode for CGPoint {
        const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
    }

    unsafe impl Encode for CGSize {
        const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
    }

    unsafe impl Encode for CGRect {
        const ENCODING: Encoding =
            Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
    }

    unsafe impl Encode for CGAffineTransform {
        const ENCODING: Encoding = Encoding::Struct(
            "?",
            &[
                f64::ENCODING,
                f64::ENCODING,
                f64::ENCODING,
                f64::ENCODING,
                f64::ENCODING,
                f64::ENCODING,
            ],
        );
    }

    pub fn present_clear(
        command_queue: &MetalCommandQueue,
        surface: &MetalSurface,
        clear: ClearColor,
    ) -> Result<(), RendererError> {
        surface.sync_drawable_size()?;
        let drawable: *mut AnyObject = unsafe { msg_send![surface.layer(), nextDrawable] };
        if drawable.is_null() {
            return Err(RendererError::Backend(
                "CAMetalLayer did not provide a drawable".to_string(),
            ));
        }

        let texture: *mut AnyObject = unsafe { msg_send![drawable, texture] };
        if texture.is_null() {
            return Err(RendererError::Backend(
                "Metal drawable did not expose a texture".to_string(),
            ));
        }

        let command_buffer: *mut AnyObject =
            unsafe { msg_send![command_queue.as_raw(), commandBuffer] };
        if command_buffer.is_null() {
            return Err(RendererError::Backend(
                "Metal command buffer creation failed".to_string(),
            ));
        }

        let render_pass_descriptor: *mut AnyObject =
            unsafe { msg_send![class!(MTLRenderPassDescriptor), renderPassDescriptor] };
        if render_pass_descriptor.is_null() {
            return Err(RendererError::Backend(
                "Metal render pass descriptor creation failed".to_string(),
            ));
        }

        let color_attachments: *mut AnyObject =
            unsafe { msg_send![render_pass_descriptor, colorAttachments] };
        if color_attachments.is_null() {
            return Err(RendererError::Backend(
                "Metal render pass color attachments are unavailable".to_string(),
            ));
        }

        let color_attachment: *mut AnyObject =
            unsafe { msg_send![color_attachments, objectAtIndexedSubscript: 0usize] };
        if color_attachment.is_null() {
            return Err(RendererError::Backend(
                "Metal render pass color attachment 0 is unavailable".to_string(),
            ));
        }

        let clear_color = MTLClearColor {
            red: clear.red,
            green: clear.green,
            blue: clear.blue,
            alpha: clear.alpha,
        };
        unsafe {
            let _: () = msg_send![color_attachment, setTexture: texture];
            let _: () = msg_send![color_attachment, setLoadAction: MTL_LOAD_ACTION_CLEAR];
            let _: () = msg_send![color_attachment, setStoreAction: MTL_STORE_ACTION_STORE];
            let _: () = msg_send![color_attachment, setClearColor: clear_color];
        }

        let encoder: *mut AnyObject = unsafe {
            msg_send![command_buffer, renderCommandEncoderWithDescriptor: render_pass_descriptor]
        };
        if encoder.is_null() {
            return Err(RendererError::Backend(
                "Metal render command encoder creation failed".to_string(),
            ));
        }

        unsafe {
            let _: () = msg_send![encoder, endEncoding];
            let _: () = msg_send![command_buffer, presentDrawable: drawable];
            let _: () = msg_send![command_buffer, commit];
        }
        Ok(())
    }

    pub enum PreparedVisual<'a> {
        SolidRect(SolidRect),
        RegisteredImage {
            texture: &'a MetalTexture,
            image: BlitImage,
        },
        GeneratedImage {
            texture: &'a MetalTexture,
            image: BlitImage,
        },
    }

    pub fn present_scene_ordered(
        command_queue: &MetalCommandQueue,
        surface: &MetalSurface,
        solid_pipeline: Option<&MetalRenderPipelineState>,
        textured_pipeline: Option<&MetalRenderPipelineState>,
        sampler: Option<&MetalSamplerState>,
        clear: ClearColor,
        visuals: &[PreparedVisual<'_>],
    ) -> Result<(), RendererError> {
        surface.sync_drawable_size()?;
        let drawable: *mut AnyObject = unsafe { msg_send![surface.layer(), nextDrawable] };
        if drawable.is_null() {
            return Err(RendererError::Backend(
                "CAMetalLayer did not provide a drawable".to_string(),
            ));
        }
        let drawable_texture: *mut AnyObject = unsafe { msg_send![drawable, texture] };
        if drawable_texture.is_null() {
            return Err(RendererError::Backend(
                "Metal drawable did not expose a texture".to_string(),
            ));
        }
        let drawable_width: u64 = unsafe { msg_send![drawable_texture, width] };
        let drawable_height: u64 = unsafe { msg_send![drawable_texture, height] };
        if drawable_width == 0 || drawable_height == 0 {
            return Err(RendererError::Backend(
                "Metal drawable had invalid size".to_string(),
            ));
        }
        let logical_size = surface.logical_size();
        let surface_width = logical_size.width.max(1.0) as f32;
        let surface_height = logical_size.height.max(1.0) as f32;

        let command_buffer: *mut AnyObject =
            unsafe { msg_send![command_queue.as_raw(), commandBuffer] };
        if command_buffer.is_null() {
            return Err(RendererError::Backend(
                "Metal command buffer creation failed".to_string(),
            ));
        }
        let render_pass_descriptor: *mut AnyObject =
            unsafe { msg_send![class!(MTLRenderPassDescriptor), renderPassDescriptor] };
        if render_pass_descriptor.is_null() {
            return Err(RendererError::Backend(
                "Metal render pass descriptor creation failed".to_string(),
            ));
        }
        let color_attachments: *mut AnyObject =
            unsafe { msg_send![render_pass_descriptor, colorAttachments] };
        if color_attachments.is_null() {
            return Err(RendererError::Backend(
                "Metal render pass color attachments are unavailable".to_string(),
            ));
        }
        let color_attachment: *mut AnyObject =
            unsafe { msg_send![color_attachments, objectAtIndexedSubscript: 0usize] };
        if color_attachment.is_null() {
            return Err(RendererError::Backend(
                "Metal render pass color attachment 0 is unavailable".to_string(),
            ));
        }
        let clear_color = MTLClearColor {
            red: clear.red,
            green: clear.green,
            blue: clear.blue,
            alpha: clear.alpha,
        };
        unsafe {
            let _: () = msg_send![color_attachment, setTexture: drawable_texture];
            let _: () = msg_send![color_attachment, setLoadAction: MTL_LOAD_ACTION_CLEAR];
            let _: () = msg_send![color_attachment, setStoreAction: MTL_STORE_ACTION_STORE];
            let _: () = msg_send![color_attachment, setClearColor: clear_color];
        }

        let encoder: *mut AnyObject = unsafe {
            msg_send![command_buffer, renderCommandEncoderWithDescriptor: render_pass_descriptor]
        };
        if encoder.is_null() {
            return Err(RendererError::Backend(
                "Metal render command encoder creation failed".to_string(),
            ));
        }

        enum ActivePipeline {
            Solid,
            Textured,
        }

        let mut active_pipeline = None;
        for visual in visuals {
            match visual {
                PreparedVisual::SolidRect(rect) => {
                    if !matches!(active_pipeline, Some(ActivePipeline::Solid)) {
                        let pipeline = solid_pipeline.ok_or_else(|| {
                            RendererError::Backend(
                                "Metal solid pipeline was unavailable for rectangle rendering"
                                    .to_string(),
                            )
                        })?;
                        unsafe {
                            let _: () =
                                msg_send![encoder, setRenderPipelineState: pipeline.as_raw()];
                        }
                        active_pipeline = Some(ActivePipeline::Solid);
                    }
                    let vertices = rect_vertices(*rect, surface_width, surface_height);
                    let color = [rect.red, rect.green, rect.blue, rect.alpha];
                    unsafe {
                        let _: () = msg_send![
                            encoder,
                            setVertexBytes: vertices.as_ptr().cast::<c_void>(),
                            length: std::mem::size_of_val(&vertices),
                            atIndex: 0usize
                        ];
                        let _: () = msg_send![
                            encoder,
                            setFragmentBytes: color.as_ptr().cast::<c_void>(),
                            length: std::mem::size_of_val(&color),
                            atIndex: 0usize
                        ];
                        let _: () = msg_send![
                            encoder,
                            drawPrimitives: MTL_PRIMITIVE_TYPE_TRIANGLE,
                            vertexStart: 0usize,
                            vertexCount: vertices.len()
                        ];
                    }
                }
                PreparedVisual::RegisteredImage { texture, image }
                | PreparedVisual::GeneratedImage { texture, image } => {
                    if !matches!(active_pipeline, Some(ActivePipeline::Textured)) {
                        let pipeline = textured_pipeline.ok_or_else(|| {
                            RendererError::Backend(
                                "Metal textured pipeline was unavailable for image rendering"
                                    .to_string(),
                            )
                        })?;
                        let sampler = sampler.ok_or_else(|| {
                            RendererError::Backend(
                                "Metal sampler was unavailable for image rendering".to_string(),
                            )
                        })?;
                        unsafe {
                            let _: () =
                                msg_send![encoder, setRenderPipelineState: pipeline.as_raw()];
                            let _: () = msg_send![
                                encoder,
                                setFragmentSamplerState: sampler.as_raw(),
                                atIndex: 0usize
                            ];
                        }
                        active_pipeline = Some(ActivePipeline::Textured);
                    }
                    let vertices = textured_rect_vertices(image, surface_width, surface_height);
                    let alpha = [image.alpha.clamp(0.0, 1.0)];
                    unsafe {
                        let _: () = msg_send![
                            encoder,
                            setVertexBytes: vertices.as_ptr().cast::<c_void>(),
                            length: std::mem::size_of_val(&vertices),
                            atIndex: 0usize
                        ];
                        let _: () = msg_send![encoder, setFragmentTexture: texture.as_raw(), atIndex: 0usize];
                        let _: () = msg_send![
                            encoder,
                            setFragmentBytes: alpha.as_ptr().cast::<c_void>(),
                            length: std::mem::size_of_val(&alpha),
                            atIndex: 0usize
                        ];
                        let _: () = msg_send![
                            encoder,
                            drawPrimitives: MTL_PRIMITIVE_TYPE_TRIANGLE,
                            vertexStart: 0usize,
                            vertexCount: vertices.len()
                        ];
                    }
                }
            }
        }

        unsafe {
            let _: () = msg_send![encoder, endEncoding];
            let _: () = msg_send![command_buffer, presentDrawable: drawable];
            let _: () = msg_send![command_buffer, commit];
        }
        Ok(())
    }

    fn rect_vertices(rect: SolidRect, surface_width: f32, surface_height: f32) -> [MetalVertex; 6] {
        let x0 = clip_x(rect.x, surface_width);
        let x1 = clip_x(rect.x + rect.width, surface_width);
        let y0 = clip_y(rect.y, surface_height);
        let y1 = clip_y(rect.y + rect.height, surface_height);
        [
            MetalVertex { x: x0, y: y0 },
            MetalVertex { x: x1, y: y0 },
            MetalVertex { x: x0, y: y1 },
            MetalVertex { x: x1, y: y0 },
            MetalVertex { x: x1, y: y1 },
            MetalVertex { x: x0, y: y1 },
        ]
    }

    fn textured_rect_vertices(
        image: &BlitImage,
        surface_width: f32,
        surface_height: f32,
    ) -> [MetalTexturedVertex; 6] {
        let mut draw_x0 = image.x;
        let mut draw_y0 = image.y;
        let mut draw_x1 = image.x + image.width;
        let mut draw_y1 = image.y + image.height;
        if let Some(clip) = image.clip_rect {
            draw_x0 = draw_x0.max(clip.x);
            draw_y0 = draw_y0.max(clip.y);
            draw_x1 = draw_x1.min(clip.x + clip.width);
            draw_y1 = draw_y1.min(clip.y + clip.height);
        }
        let safe_width = image.width.max(1.0);
        let safe_height = image.height.max(1.0);
        let u0 = ((draw_x0 - image.x) / safe_width).clamp(0.0, 1.0);
        let u1 = ((draw_x1 - image.x) / safe_width).clamp(0.0, 1.0);
        let raw_v0 = ((draw_y0 - image.y) / safe_height).clamp(0.0, 1.0);
        let raw_v1 = ((draw_y1 - image.y) / safe_height).clamp(0.0, 1.0);
        let x0 = clip_x(draw_x0, surface_width);
        let x1 = clip_x(draw_x1, surface_width);
        let y0 = clip_y(draw_y0, surface_height);
        let y1 = clip_y(draw_y1, surface_height);
        let (v0, v1) = if image.flip_vertical {
            (1.0 - raw_v0, 1.0 - raw_v1)
        } else {
            (raw_v0, raw_v1)
        };
        [
            MetalTexturedVertex {
                x: x0,
                y: y0,
                u: u0,
                v: v0,
            },
            MetalTexturedVertex {
                x: x1,
                y: y0,
                u: u1,
                v: v0,
            },
            MetalTexturedVertex {
                x: x0,
                y: y1,
                u: u0,
                v: v1,
            },
            MetalTexturedVertex {
                x: x1,
                y: y0,
                u: u1,
                v: v0,
            },
            MetalTexturedVertex {
                x: x1,
                y: y1,
                u: u1,
                v: v1,
            },
            MetalTexturedVertex {
                x: x0,
                y: y1,
                u: u0,
                v: v1,
            },
        ]
    }

    fn clip_x(x: f32, width: f32) -> f32 {
        (x / width) * 2.0 - 1.0
    }

    fn clip_y(y: f32, height: f32) -> f32 {
        1.0 - (y / height) * 2.0
    }

    fn ns_string(value: &str) -> Result<Retained<AnyObject>, RendererError> {
        let cstr = CString::new(value).map_err(|err| {
            RendererError::Backend(format!("failed to build NSString source: {err}"))
        })?;
        let string: *mut AnyObject =
            unsafe { msg_send![class!(NSString), stringWithUTF8String: cstr.as_ptr()] };
        unsafe { Retained::retain(string) }
            .ok_or_else(|| RendererError::Backend("NSString allocation failed".to_string()))
    }

    fn library_error_message(error: *mut AnyObject) -> String {
        if error.is_null() {
            return "Metal shader compilation failed".to_string();
        }
        let description: *mut AnyObject = unsafe { msg_send![error, localizedDescription] };
        if description.is_null() {
            return "Metal shader compilation failed".to_string();
        }
        let utf8: *const i8 = unsafe { msg_send![description, UTF8String] };
        if utf8.is_null() {
            return "Metal shader compilation failed".to_string();
        }
        unsafe { std::ffi::CStr::from_ptr(utf8) }
            .to_string_lossy()
            .into_owned()
    }

    fn shader_library(
        device: &MetalDevice,
        source_text: &str,
    ) -> Result<Retained<AnyObject>, RendererError> {
        let source = ns_string(source_text)?;
        let mut error: *mut AnyObject = std::ptr::null_mut();
        let library: Option<Retained<AnyObject>> = unsafe {
            msg_send![
                device.as_raw(),
                newLibraryWithSource: &*source,
                options: std::ptr::null_mut::<AnyObject>(),
                error: &mut error
            ]
        };
        library.ok_or_else(|| RendererError::Backend(library_error_message(error)))
    }
}

impl GraphicsBackend for MetalBackend {
    fn begin_frame(&mut self) -> Result<(), RendererError> {
        self.ensure_bound()?;
        self.recorded_commands.clear();
        self.frame_open = true;
        Ok(())
    }

    fn submit(&mut self, commands: &[FrameCommand]) -> Result<(), RendererError> {
        self.ensure_bound()?;
        if !self.frame_open {
            return Err(RendererError::Backend(
                "submit called before begin_frame".to_string(),
            ));
        }
        self.recorded_commands.extend_from_slice(commands);
        Ok(())
    }

    fn end_frame(&mut self) -> Result<(), RendererError> {
        self.ensure_bound()?;
        if !self.frame_open {
            return Err(RendererError::Backend(
                "end_frame called before begin_frame".to_string(),
            ));
        }
        #[cfg(target_os = "macos")]
        if self.state == MetalBackendState::SurfaceBound {
            let clear = self.frame_clear_color().unwrap_or(ClearColor {
                red: 0.0,
                green: 0.0,
                blue: 0.0,
                alpha: 1.0,
            });
            let visuals = self.frame_visuals()?;
            let has_textured_images = visuals.iter().any(|visual| {
                matches!(
                    visual,
                    FrameVisual::RegisteredImage(_) | FrameVisual::GeneratedImage(_)
                )
            });
            if visuals.is_empty() {
                let command_queue = self.command_queue.as_ref().ok_or_else(|| {
                    RendererError::Backend("Metal command queue is unavailable".to_string())
                })?;
                let surface = self.surface.as_ref().ok_or_else(|| {
                    RendererError::Backend("Metal surface is unavailable".to_string())
                })?;
                macos::present_clear(command_queue, surface, clear)?;
            } else {
                if self.pipeline_state.is_none() {
                    let device = self.device.as_ref().ok_or_else(|| {
                        RendererError::Backend("Metal device is unavailable".to_string())
                    })?;
                    self.pipeline_state = Some(macos::MetalRenderPipelineState::new_solid(device)?);
                }
                if has_textured_images && self.textured_pipeline_state.is_none() {
                    let device = self.device.as_ref().ok_or_else(|| {
                        RendererError::Backend("Metal device is unavailable".to_string())
                    })?;
                    self.textured_pipeline_state =
                        Some(macos::MetalRenderPipelineState::new_textured(device)?);
                }
                if has_textured_images && self.sampler_state.is_none() {
                    let device = self.device.as_ref().ok_or_else(|| {
                        RendererError::Backend("Metal device is unavailable".to_string())
                    })?;
                    self.sampler_state = Some(macos::MetalSamplerState::new_linear(device)?);
                }
                self.ensure_image_resources()?;
                let command_queue = self.command_queue.as_ref().ok_or_else(|| {
                    RendererError::Backend("Metal command queue is unavailable".to_string())
                })?;
                let surface = self.surface.as_ref().ok_or_else(|| {
                    RendererError::Backend("Metal surface is unavailable".to_string())
                })?;
                let device = self.device.as_ref().ok_or_else(|| {
                    RendererError::Backend("Metal device is unavailable".to_string())
                })?;
                let generated_images = visuals
                    .iter()
                    .filter_map(|visual| match visual {
                        FrameVisual::GeneratedImage(image) => Some(image.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let transient_textures = generated_images
                    .iter()
                    .enumerate()
                    .map(|(index, image)| {
                        let key = format!("__loadngo_generated_{index}");
                        let texture =
                            macos::MetalTexture::from_decoded_image(device, &key, &image.image)?;
                        Ok((texture, image.placement.clone()))
                    })
                    .collect::<Result<Vec<_>, RendererError>>()?;
                let mut generated_index = 0usize;
                let prepared_visuals = visuals
                    .iter()
                    .map(|visual| match visual {
                        FrameVisual::SolidRect(rect) => Ok(macos::PreparedVisual::SolidRect(*rect)),
                        FrameVisual::RegisteredImage(image) => {
                            let texture = self.textures.get(&image.image_key).ok_or_else(|| {
                                RendererError::Backend(format!(
                                    "Metal texture was not loaded for {}",
                                    image.image_key
                                ))
                            })?;
                            Ok(macos::PreparedVisual::RegisteredImage {
                                texture,
                                image: image.clone(),
                            })
                        }
                        FrameVisual::GeneratedImage(_) => {
                            let (texture, image) =
                                transient_textures.get(generated_index).ok_or_else(|| {
                                    RendererError::Backend(
                                        "generated texture ordering mismatch".to_string(),
                                    )
                                })?;
                            generated_index += 1;
                            Ok(macos::PreparedVisual::GeneratedImage {
                                texture,
                                image: image.clone(),
                            })
                        }
                    })
                    .collect::<Result<Vec<_>, RendererError>>()?;
                macos::present_scene_ordered(
                    command_queue,
                    surface,
                    self.pipeline_state.as_ref(),
                    self.textured_pipeline_state.as_ref(),
                    self.sampler_state.as_ref(),
                    clear,
                    &prepared_visuals,
                )?;
            }
        }
        self.frame_open = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    use loadngo_renderer::TextRequest;
    use loadngo_renderer::{FrameCommand, Renderer, RendererConfig};
    use ui_core::geometry::Color;

    #[test]
    fn unbound_backend_rejects_frames() {
        let mut backend = MetalBackend::new();
        let err = backend
            .begin_frame()
            .expect_err("unbound backend should reject rendering");
        match err {
            RendererError::Backend(message) => {
                assert!(message.contains("not bound"));
            }
            other => panic!("expected backend error, got {other:?}"),
        }
    }

    #[test]
    fn headless_backend_records_frame_commands() {
        let renderer = Renderer::new(RendererConfig::default());
        let commands = vec![FrameCommand::Clear {
            color: Color::rgba(1, 2, 3, 255),
        }];
        let mut backend = MetalBackend::new_headless();
        renderer
            .render(&mut backend, &commands)
            .expect("headless backend should accept commands");
        assert_eq!(backend.take_recorded_commands(), commands);
    }

    #[test]
    fn frame_clear_color_prefers_last_clear_command() {
        let mut backend = MetalBackend::new_headless();
        backend.recorded_commands = vec![
            FrameCommand::Clear {
                color: Color::rgba(10, 20, 30, 255),
            },
            FrameCommand::FillRect {
                rect: ui_core::geometry::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 10.0,
                },
                color: Color::rgba(255, 0, 0, 255),
            },
            FrameCommand::Clear {
                color: Color::rgba(40, 50, 60, 128),
            },
        ];
        assert_eq!(
            backend.frame_clear_color(),
            Some(ClearColor {
                red: 40.0 / 255.0,
                green: 50.0 / 255.0,
                blue: 60.0 / 255.0,
                alpha: 128.0 / 255.0,
            })
        );
    }

    #[test]
    fn stroke_rect_expands_to_four_solid_rects() {
        let mut backend = MetalBackend::new_headless();
        backend.recorded_commands = vec![FrameCommand::StrokeRect {
            rect: ui_core::geometry::Rect {
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 40.0,
            },
            color: Color::rgba(255, 255, 255, 255),
            thickness: 4,
        }];
        let rects = backend.frame_solid_rects();
        assert_eq!(rects.len(), 4);
        assert!(rects.iter().all(|rect| rect.alpha > 0.0));
    }

    #[test]
    fn rasterized_line_generates_pixels() {
        let image = rasterize_line(
            ui_core::geometry::Point { x: 0.0, y: 0.0 },
            ui_core::geometry::Point { x: 8.0, y: 0.0 },
            Color::rgba(255, 0, 0, 255),
            2,
        )
        .expect("line should rasterize");
        assert!(image.image.rgba8.iter().any(|byte| *byte != 0));
    }

    #[test]
    fn rasterized_circle_generates_pixels() {
        let image = rasterize_circle(
            ui_core::geometry::Point { x: 8.0, y: 8.0 },
            4,
            Color::rgba(0, 255, 0, 255),
        )
        .expect("circle should rasterize");
        assert!(image.image.rgba8.iter().any(|byte| *byte != 0));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_backend_binds_system_default_device() {
        let backend = MetalBackend::try_bind_system_default()
            .expect("macOS should expose a default Metal device");
        assert_eq!(backend.state(), MetalBackendState::Ready);
        assert!(backend.has_bound_device());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_text_raster_is_not_double_flipped() {
        fn row_alpha_counts(image: &DecodedImage, rows: usize) -> Vec<usize> {
            let width = image.width as usize;
            let height = image.height as usize;
            let rows = rows.min(height);
            (0..rows)
                .map(|y| {
                    let start = y * width * 4;
                    let end = start + width * 4;
                    image.rgba8[start..end]
                        .chunks_exact(4)
                        .filter(|px| px[3] > 0)
                        .count()
                })
                .collect()
        }

        for (text, rect_height, centered) in [
            ("Menu", 44, true),
            ("Labels", 24, false),
            ("Live Preview", 30, false),
        ] {
            let request = TextRequest {
                rect: ui_core::geometry::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 300.0,
                    height: rect_height as f32,
                },
                text: text.to_string(),
                style: loadngo_host_core::RenderTextStyle {
                    horizontal_align: if centered {
                        loadngo_host_core::RenderTextHorizontalAlign::Center
                    } else {
                        loadngo_host_core::RenderTextHorizontalAlign::Left
                    },
                    vertical_align: if centered {
                        loadngo_host_core::RenderTextVerticalAlign::Middle
                    } else {
                        loadngo_host_core::RenderTextVerticalAlign::Top
                    },
                    layout_mode: loadngo_host_core::RenderTextLayoutMode::SingleLine,
                    overflow: loadngo_host_core::RenderTextOverflow::Clip,
                    ..Default::default()
                },
                direction: loadngo_renderer::TextDirection::Auto,
                script: loadngo_renderer::TextScript::Auto,
                language: None,
            };
            let raster =
                rasterize_text_request(&request, None).expect("text raster should succeed");
            let top_rows = row_alpha_counts(&raster.image, 8);
            let mut all_rows = row_alpha_counts(&raster.image, raster.image.height as usize);
            all_rows.reverse();
            let bottom_rows = all_rows.into_iter().take(8).collect::<Vec<_>>();
            let total_alpha_rows = row_alpha_counts(&raster.image, raster.image.height as usize)
                .into_iter()
                .filter(|count| *count > 0)
                .count();
            assert!(
                total_alpha_rows > 0,
                "raw text raster unexpectedly has no alpha for {text}"
            );
            assert!(
                top_rows.iter().any(|count| *count > 0) || bottom_rows.iter().any(|count| *count > 0),
                "raw text raster unexpectedly lacks edge alpha for {text}: top={top_rows:?} bottom={bottom_rows:?}"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn generated_text_visuals_flip_vertically() {
        let mut backend = MetalBackend::new_headless();
        backend.recorded_commands = vec![FrameCommand::Text(TextRequest {
            rect: ui_core::geometry::Rect {
                x: 10.0,
                y: 20.0,
                width: 120.0,
                height: 44.0,
            },
            text: "Menu".to_string(),
            style: loadngo_host_core::RenderTextStyle {
                horizontal_align: loadngo_host_core::RenderTextHorizontalAlign::Center,
                vertical_align: loadngo_host_core::RenderTextVerticalAlign::Middle,
                layout_mode: loadngo_host_core::RenderTextLayoutMode::SingleLine,
                overflow: loadngo_host_core::RenderTextOverflow::Clip,
                ..Default::default()
            },
            direction: loadngo_renderer::TextDirection::Auto,
            script: loadngo_renderer::TextScript::Auto,
            language: None,
        })];
        let visuals = backend
            .frame_visuals()
            .expect("text command should rasterize into a visual");
        let text_visual = visuals
            .into_iter()
            .find_map(|visual| match visual {
                FrameVisual::GeneratedImage(image) => Some(image),
                _ => None,
            })
            .expect("expected generated text image");
        assert!(text_visual.placement.flip_vertical);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn top_aligned_text_aligns_visible_top_to_rect_top() {
        let request = TextRequest {
            rect: ui_core::geometry::Rect {
                x: 0.0,
                y: 0.0,
                width: 300.0,
                height: 24.0,
            },
            text: "Labels".to_string(),
            style: loadngo_host_core::RenderTextStyle {
                horizontal_align: loadngo_host_core::RenderTextHorizontalAlign::Left,
                vertical_align: loadngo_host_core::RenderTextVerticalAlign::Top,
                layout_mode: loadngo_host_core::RenderTextLayoutMode::SingleLine,
                overflow: loadngo_host_core::RenderTextOverflow::Clip,
                ..Default::default()
            },
            direction: loadngo_renderer::TextDirection::Auto,
            script: loadngo_renderer::TextScript::Auto,
            language: None,
        };
        let raster = rasterize_text_request(&request, None).expect("text raster should succeed");
        let displayed_visible_top = raster.y + raster.opaque_top_in_display;
        assert!(
            displayed_visible_top.abs() < 0.5,
            "expected visible text top to align with rect top, got {displayed_visible_top}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn middle_aligned_text_centers_visible_text_in_rect() {
        let request = TextRequest {
            rect: ui_core::geometry::Rect {
                x: 10.0,
                y: 20.0,
                width: 220.0,
                height: 60.0,
            },
            text: "Menu".to_string(),
            style: loadngo_host_core::RenderTextStyle {
                horizontal_align: loadngo_host_core::RenderTextHorizontalAlign::Center,
                vertical_align: loadngo_host_core::RenderTextVerticalAlign::Middle,
                layout_mode: loadngo_host_core::RenderTextLayoutMode::SingleLine,
                overflow: loadngo_host_core::RenderTextOverflow::Clip,
                ..Default::default()
            },
            direction: loadngo_renderer::TextDirection::Auto,
            script: loadngo_renderer::TextScript::Auto,
            language: None,
        };
        let raster = rasterize_text_request(&request, None).expect("text raster should succeed");
        let expected_x =
            request.rect.x + (request.rect.width - raster.metrics.width).max(0.0) * 0.5;
        let displayed_visible_top = raster.y + raster.opaque_top_in_display;
        let expected_top =
            request.rect.y + (request.rect.height - raster.opaque_height).max(0.0) * 0.5;
        assert!((raster.x - expected_x).abs() < 0.5);
        assert!((displayed_visible_top - expected_top).abs() < 0.5);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn bottom_aligned_text_positions_visible_bottom_at_rect_bottom() {
        let request = TextRequest {
            rect: ui_core::geometry::Rect {
                x: 0.0,
                y: 0.0,
                width: 220.0,
                height: 48.0,
            },
            text: "Inspector".to_string(),
            style: loadngo_host_core::RenderTextStyle {
                horizontal_align: loadngo_host_core::RenderTextHorizontalAlign::Left,
                vertical_align: loadngo_host_core::RenderTextVerticalAlign::Bottom,
                layout_mode: loadngo_host_core::RenderTextLayoutMode::SingleLine,
                overflow: loadngo_host_core::RenderTextOverflow::Clip,
                ..Default::default()
            },
            direction: loadngo_renderer::TextDirection::Auto,
            script: loadngo_renderer::TextScript::Auto,
            language: None,
        };
        let raster = rasterize_text_request(&request, None).expect("text raster should succeed");
        let displayed_logical_bottom =
            raster.y + raster.opaque_top_in_display + raster.opaque_height;
        assert!((displayed_logical_bottom - request.rect.height).abs() < 0.5);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn single_line_text_reports_shared_line_box_height() {
        let request = TextRequest {
            rect: ui_core::geometry::Rect {
                x: 0.0,
                y: 0.0,
                width: 240.0,
                height: 44.0,
            },
            text: "Menu".to_string(),
            style: loadngo_host_core::RenderTextStyle {
                font_size: 18,
                horizontal_align: loadngo_host_core::RenderTextHorizontalAlign::Center,
                vertical_align: loadngo_host_core::RenderTextVerticalAlign::Middle,
                layout_mode: loadngo_host_core::RenderTextLayoutMode::SingleLine,
                overflow: loadngo_host_core::RenderTextOverflow::Clip,
                ..Default::default()
            },
            direction: loadngo_renderer::TextDirection::Auto,
            script: loadngo_renderer::TextScript::Auto,
            language: None,
        };

        let raster = rasterize_text_request(&request, None).expect("text raster should succeed");

        assert_eq!(
            raster.metrics.height,
            ui_core::single_line_text_box_height(18)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn single_line_text_raster_trims_excess_vertical_padding() {
        let request = TextRequest {
            rect: ui_core::geometry::Rect {
                x: 0.0,
                y: 0.0,
                width: 240.0,
                height: 44.0,
            },
            text: "Menu".to_string(),
            style: loadngo_host_core::RenderTextStyle {
                font_size: 18,
                horizontal_align: loadngo_host_core::RenderTextHorizontalAlign::Center,
                vertical_align: loadngo_host_core::RenderTextVerticalAlign::Middle,
                layout_mode: loadngo_host_core::RenderTextLayoutMode::SingleLine,
                overflow: loadngo_host_core::RenderTextOverflow::Clip,
                ..Default::default()
            },
            direction: loadngo_renderer::TextDirection::Auto,
            script: loadngo_renderer::TextScript::Auto,
            language: None,
        };

        let raster = rasterize_text_request(&request, None).expect("text raster should succeed");

        assert!(raster.image.height as f32 <= raster.metrics.height + 8.0);
        assert!(
            (raster.y
                - (request.rect.y + (request.rect.height - raster.opaque_height) * 0.5
                    - raster.opaque_top_in_display))
                .abs()
                < 0.5
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn single_line_overflow_policies_fit_within_requested_width() {
        let max_width = 84.0;
        let clip = apply_single_line_overflow(
            "TallahasseeDawnPreviewLabel",
            max_width,
            None,
            18.0,
            &loadngo_host_core::RenderTextOverflow::Clip,
        )
        .expect("clip overflow should succeed");
        let ellipsis_end = apply_single_line_overflow(
            "TallahasseeDawnPreviewLabel",
            max_width,
            None,
            18.0,
            &loadngo_host_core::RenderTextOverflow::EllipsisEnd,
        )
        .expect("end overflow should succeed");
        let ellipsis_middle = apply_single_line_overflow(
            "TallahasseeDawnPreviewLabel",
            max_width,
            None,
            18.0,
            &loadngo_host_core::RenderTextOverflow::EllipsisMiddle,
        )
        .expect("middle overflow should succeed");

        assert!(!clip.contains("..."));
        assert!(ellipsis_end.ends_with("..."));
        assert!(ellipsis_middle.contains("..."));
        assert!(!ellipsis_middle.starts_with("..."));
        assert!(!ellipsis_middle.ends_with("..."));

        for rendered in [clip, ellipsis_end, ellipsis_middle] {
            let measured = measure_text_metrics(&rendered, None, 18.0)
                .expect("overflow result should be measurable");
            assert!(
                measured.width <= max_width + 0.5,
                "rendered='{rendered}' width={}",
                measured.width
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn multiline_text_reports_taller_logical_height_than_single_line() {
        let single = TextRequest {
            rect: ui_core::geometry::Rect {
                x: 0.0,
                y: 0.0,
                width: 320.0,
                height: 80.0,
            },
            text: "Silver and Gold".to_string(),
            style: loadngo_host_core::RenderTextStyle {
                layout_mode: loadngo_host_core::RenderTextLayoutMode::SingleLine,
                overflow: loadngo_host_core::RenderTextOverflow::Clip,
                ..Default::default()
            },
            direction: loadngo_renderer::TextDirection::Auto,
            script: loadngo_renderer::TextScript::Auto,
            language: None,
        };
        let multi = TextRequest {
            text: "Silver\nand\nGold".to_string(),
            style: loadngo_host_core::RenderTextStyle {
                layout_mode: loadngo_host_core::RenderTextLayoutMode::MultiLine,
                overflow: loadngo_host_core::RenderTextOverflow::Clip,
                ..Default::default()
            },
            ..single.clone()
        };

        let single_raster =
            rasterize_text_request(&single, None).expect("single line should rasterize");
        let multi_raster =
            rasterize_text_request(&multi, None).expect("multi line should rasterize");
        assert!(multi_raster.metrics.height > single_raster.metrics.height);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn multiline_text_preserves_top_to_bottom_line_order_in_display_space() {
        let request = TextRequest {
            rect: ui_core::geometry::Rect {
                x: 0.0,
                y: 0.0,
                width: 400.0,
                height: 160.0,
            },
            text: "WWWWWWWWWWWW\nMMMMMM\nHH".to_string(),
            style: loadngo_host_core::RenderTextStyle {
                font_size: 22,
                layout_mode: loadngo_host_core::RenderTextLayoutMode::MultiLine,
                overflow: loadngo_host_core::RenderTextOverflow::Clip,
                ..Default::default()
            },
            direction: loadngo_renderer::TextDirection::Auto,
            script: loadngo_renderer::TextScript::Auto,
            language: None,
        };

        let raster = rasterize_text_request(&request, None).expect("multiline should rasterize");
        let band_widths = displayed_opaque_band_widths(&raster.image);

        assert_eq!(band_widths.len(), 3, "expected three displayed text bands");
        assert!(
            band_widths[0] > band_widths[1],
            "top band should correspond to widest first line: {:?}",
            band_widths
        );
        assert!(
            band_widths[1] > band_widths[2],
            "middle band should correspond to medium second line: {:?}",
            band_widths
        );
    }

    #[cfg(target_os = "macos")]
    fn displayed_opaque_band_widths(image: &DecodedImage) -> Vec<usize> {
        let mut row_widths = Vec::with_capacity(image.height as usize);
        for display_row in 0..image.height as usize {
            let image_row = image.height as usize - 1 - display_row;
            let row_start = image_row * image.width as usize * 4;
            let row_end = row_start + image.width as usize * 4;
            let width = image.rgba8[row_start..row_end]
                .chunks_exact(4)
                .filter(|pixel| pixel[3] > 0)
                .count();
            row_widths.push(width);
        }

        let mut bands = Vec::new();
        let mut in_band = false;
        let mut band_max = 0usize;
        for width in row_widths {
            if width > 0 {
                in_band = true;
                band_max = band_max.max(width);
            } else if in_band {
                bands.push(band_max);
                in_band = false;
                band_max = 0;
            }
        }
        if in_band {
            bands.push(band_max);
        }
        bands
    }
}
