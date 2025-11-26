use std::{
    f32::consts::TAU,
    path::{Path, PathBuf},
};

use loadngo_host_core::{RenderOp, RenderTextStyle};
use serde::{Deserialize, Serialize};
use ui_core::{
    geometry::{Color, Point, Rect},
    paint::{PaintOp, Particle},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextDirection {
    Auto,
    LeftToRight,
    RightToLeft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextScript {
    Auto,
    Latin,
    Cyrillic,
    Greek,
    Arabic,
    Hebrew,
    Devanagari,
    Thai,
    Han,
    Hiragana,
    Katakana,
    Hangul,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageTag(String);

impl LanguageTag {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePlatform {
    MacOs,
    Ios,
    Android,
    Windows,
    Linux,
    Unknown,
}

impl RuntimePlatform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "ios") {
            Self::Ios
        } else if cfg!(target_os = "android") {
            Self::Android
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Unknown
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PlatformFontPaths {
    #[serde(default)]
    pub macos: Vec<String>,
    #[serde(default)]
    pub ios: Vec<String>,
    #[serde(default)]
    pub android: Vec<String>,
    #[serde(default)]
    pub windows: Vec<String>,
    #[serde(default)]
    pub linux: Vec<String>,
}

impl PlatformFontPaths {
    pub fn for_platform(&self, platform: RuntimePlatform) -> &[String] {
        match platform {
            RuntimePlatform::MacOs => &self.macos,
            RuntimePlatform::Ios => &self.ios,
            RuntimePlatform::Android => &self.android,
            RuntimePlatform::Windows => &self.windows,
            RuntimePlatform::Linux => &self.linux,
            RuntimePlatform::Unknown => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PlatformFontFamilies {
    #[serde(default)]
    pub macos: Vec<String>,
    #[serde(default)]
    pub ios: Vec<String>,
    #[serde(default)]
    pub android: Vec<String>,
    #[serde(default)]
    pub windows: Vec<String>,
    #[serde(default)]
    pub linux: Vec<String>,
}

impl PlatformFontFamilies {
    pub fn for_platform(&self, platform: RuntimePlatform) -> &[String] {
        match platform {
            RuntimePlatform::MacOs => &self.macos,
            RuntimePlatform::Ios => &self.ios,
            RuntimePlatform::Android => &self.android,
            RuntimePlatform::Windows => &self.windows,
            RuntimePlatform::Linux => &self.linux,
            RuntimePlatform::Unknown => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FontFaceManifest {
    #[serde(default)]
    pub family_name: Option<String>,
    #[serde(default)]
    pub asset_rel_paths: Vec<String>,
    #[serde(default)]
    pub platform_paths: PlatformFontPaths,
    #[serde(default)]
    pub platform_families: PlatformFontFamilies,
}

impl FontFaceManifest {
    fn push_asset_candidates(
        &self,
        assets_root: &Path,
        seen: &mut std::collections::HashSet<PathBuf>,
        out: &mut Vec<PathBuf>,
    ) {
        for rel_path in &self.asset_rel_paths {
            let candidate = assets_root.join("fonts").join(rel_path);
            if candidate.exists() && seen.insert(candidate.clone()) {
                out.push(candidate);
            }
        }
    }

    fn push_platform_candidates(
        &self,
        platform: RuntimePlatform,
        seen: &mut std::collections::HashSet<PathBuf>,
        out: &mut Vec<PathBuf>,
    ) {
        for platform_path in self.platform_paths.for_platform(platform) {
            let candidate = PathBuf::from(platform_path);
            if candidate.exists() && seen.insert(candidate.clone()) {
                out.push(candidate);
            }
        }
    }

    pub fn candidate_paths(&self, assets_root: &Path, platform: RuntimePlatform) -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let prefer_platform_fonts =
            matches!(platform, RuntimePlatform::Android | RuntimePlatform::Ios);

        if prefer_platform_fonts {
            self.push_platform_candidates(platform, &mut seen, &mut candidates);
            self.push_asset_candidates(assets_root, &mut seen, &mut candidates);
        } else {
            self.push_asset_candidates(assets_root, &mut seen, &mut candidates);
            self.push_platform_candidates(platform, &mut seen, &mut candidates);
        }

        candidates
    }

    pub fn resolve_path(&self, assets_root: &Path, platform: RuntimePlatform) -> Option<PathBuf> {
        self.candidate_paths(assets_root, platform)
            .into_iter()
            .next()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FontCatalogManifest {
    #[serde(default)]
    pub novel_font: FontFaceManifest,
    #[serde(default)]
    pub ui_font: Option<FontFaceManifest>,
    #[serde(default)]
    pub fallback_fonts: Vec<FontFaceManifest>,
}

impl FontCatalogManifest {
    pub fn resolve_novel_font_paths(
        &self,
        assets_root: &Path,
        platform: RuntimePlatform,
    ) -> Vec<PathBuf> {
        let mut candidates = self.novel_font.candidate_paths(assets_root, platform);
        let mut seen: std::collections::HashSet<PathBuf> = candidates.iter().cloned().collect();

        if let Some(ui_font) = &self.ui_font {
            for candidate in ui_font.candidate_paths(assets_root, platform) {
                if seen.insert(candidate.clone()) {
                    candidates.push(candidate);
                }
            }
        }

        for fallback in &self.fallback_fonts {
            for candidate in fallback.candidate_paths(assets_root, platform) {
                if seen.insert(candidate.clone()) {
                    candidates.push(candidate);
                }
            }
        }

        candidates
    }

    pub fn resolve_novel_font_path(
        &self,
        assets_root: &Path,
        platform: RuntimePlatform,
    ) -> Option<PathBuf> {
        self.resolve_novel_font_paths(assets_root, platform)
            .into_iter()
            .next()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextRequest {
    pub rect: Rect,
    pub clip_rect: Option<Rect>,
    pub text: String,
    pub style: RenderTextStyle,
    pub font_source: Option<String>,
    pub direction: TextDirection,
    pub script: TextScript,
    pub language: Option<LanguageTag>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageRequest {
    pub rect: Rect,
    pub clip_rect: Option<Rect>,
    pub image_key: String,
    pub alpha: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageResourceKey(String);

impl ImageResourceKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FrameResourcePlan {
    pub image_keys: Vec<ImageResourceKey>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FrameCommand {
    Clear {
        color: Color,
    },
    FillRect {
        rect: Rect,
        color: Color,
    },
    StrokeRect {
        rect: Rect,
        color: Color,
        thickness: i32,
    },
    Line {
        from: Point,
        to: Point,
        color: Color,
        thickness: i32,
    },
    Circle {
        center: Point,
        radius: f32,
        color: Color,
    },
    Polyline {
        points: Vec<Point>,
        color: Color,
        thickness: i32,
        closed: bool,
    },
    Arc {
        center: Point,
        radius: f32,
        start_angle: f32,
        sweep_angle: f32,
        color: Color,
        thickness: i32,
    },
    ParticleBatch {
        particles: Vec<Particle>,
    },
    Text(TextRequest),
    Image(ImageRequest),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RendererError {
    Backend(String),
    Text(String),
}

impl std::fmt::Display for RendererError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RendererError::Backend(message) => write!(f, "backend error: {message}"),
            RendererError::Text(message) => write!(f, "text error: {message}"),
        }
    }
}

impl std::error::Error for RendererError {}

#[derive(Debug, Clone, PartialEq)]
pub struct TextLayoutRequest<F> {
    pub text: String,
    pub font: Option<F>,
    pub font_size: u16,
    pub bounds: Option<Rect>,
    pub direction: TextDirection,
    pub script: TextScript,
    pub language: Option<LanguageTag>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextMeasurement {
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShapedGlyph {
    pub glyph_id: u32,
    pub cluster: usize,
    pub x: f32,
    pub y: f32,
    pub advance: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShapedTextRun<F> {
    pub text: String,
    pub font: Option<F>,
    pub glyphs: Vec<ShapedGlyph>,
    pub direction: TextDirection,
    pub script: TextScript,
    pub language: Option<LanguageTag>,
}

pub trait TextEngine {
    type FontHandle: Clone;

    fn measure(
        &mut self,
        request: &TextLayoutRequest<Self::FontHandle>,
    ) -> Result<TextMeasurement, RendererError>;

    fn shape(
        &mut self,
        request: &TextLayoutRequest<Self::FontHandle>,
    ) -> Result<ShapedTextRun<Self::FontHandle>, RendererError>;
}

pub trait GraphicsBackend {
    fn begin_frame(&mut self) -> Result<(), RendererError>;
    fn submit(&mut self, commands: &[FrameCommand]) -> Result<(), RendererError>;
    fn end_frame(&mut self) -> Result<(), RendererError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererConfig {
    pub default_language: Option<LanguageTag>,
    pub default_direction: TextDirection,
    pub default_script: TextScript,
    pub widget_font_size: u16,
    pub widget_stroke_thickness: i32,
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            default_language: None,
            default_direction: TextDirection::Auto,
            default_script: TextScript::Auto,
            widget_font_size: 18,
            widget_stroke_thickness: 2,
        }
    }
}

pub struct Renderer {
    config: RendererConfig,
}

impl Renderer {
    pub fn new(config: RendererConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &RendererConfig {
        &self.config
    }

    pub fn encode_render_ops(&self, ops: &[RenderOp]) -> Vec<FrameCommand> {
        ops.iter()
            .map(|op| match op {
                RenderOp::Clear { color } => FrameCommand::Clear { color: *color },
                RenderOp::FillRect { rect, color } => FrameCommand::FillRect {
                    rect: *rect,
                    color: *color,
                },
                RenderOp::StrokeRect {
                    rect,
                    color,
                    thickness,
                } => FrameCommand::StrokeRect {
                    rect: *rect,
                    color: *color,
                    thickness: *thickness,
                },
                RenderOp::Line {
                    from,
                    to,
                    color,
                    thickness,
                } => FrameCommand::Line {
                    from: *from,
                    to: *to,
                    color: *color,
                    thickness: *thickness,
                },
                RenderOp::Circle {
                    center,
                    radius,
                    color,
                } => FrameCommand::Circle {
                    center: *center,
                    radius: *radius,
                    color: *color,
                },
                RenderOp::Text { rect, text, style } => {
                    FrameCommand::Text(self.text_request(*rect, None, text.clone(), style.clone()))
                }
                RenderOp::BlitImage {
                    rect,
                    image_key,
                    alpha,
                } => FrameCommand::Image(ImageRequest {
                    rect: *rect,
                    clip_rect: None,
                    image_key: image_key.clone(),
                    alpha: *alpha,
                }),
            })
            .collect()
    }

    pub fn encode_paint_ops(&self, ops: &[PaintOp]) -> Vec<FrameCommand> {
        let mut commands = Vec::new();
        for op in ops {
            match op {
                PaintOp::FillRect { rect, color } => commands.push(FrameCommand::FillRect {
                    rect: *rect,
                    color: *color,
                }),
                PaintOp::StrokeRect { rect, color } => commands.push(FrameCommand::StrokeRect {
                    rect: *rect,
                    color: *color,
                    thickness: self.config.widget_stroke_thickness,
                }),
                PaintOp::Line { from, to, color } => commands.push(FrameCommand::Line {
                    from: *from,
                    to: *to,
                    color: *color,
                    thickness: self.config.widget_stroke_thickness,
                }),
                PaintOp::FillCircle {
                    center,
                    radius,
                    color,
                } => commands.push(FrameCommand::Circle {
                    center: *center,
                    radius: *radius,
                    color: *color,
                }),
                PaintOp::StrokeCircle {
                    center,
                    radius,
                    color,
                    thickness,
                } => commands.push(FrameCommand::Polyline {
                    points: approximate_arc_points(*center, *radius, 0.0, TAU),
                    color: *color,
                    thickness: *thickness,
                    closed: true,
                }),
                PaintOp::Polyline {
                    points,
                    color,
                    thickness,
                    closed,
                } => commands.push(FrameCommand::Polyline {
                    points: points.clone(),
                    color: *color,
                    thickness: *thickness,
                    closed: *closed,
                }),
                PaintOp::Arc {
                    center,
                    radius,
                    start_angle,
                    sweep_angle,
                    color,
                    thickness,
                } => commands.push(FrameCommand::Arc {
                    center: *center,
                    radius: *radius,
                    start_angle: *start_angle,
                    sweep_angle: *sweep_angle,
                    color: *color,
                    thickness: *thickness,
                }),
                PaintOp::QuadraticBezier {
                    start,
                    control,
                    end,
                    color,
                    thickness,
                } => commands.push(FrameCommand::Polyline {
                    points: approximate_quadratic_points(*start, *control, *end),
                    color: *color,
                    thickness: *thickness,
                    closed: false,
                }),
                PaintOp::CubicBezier {
                    start,
                    control1,
                    control2,
                    end,
                    color,
                    thickness,
                } => commands.push(FrameCommand::Polyline {
                    points: approximate_cubic_points(*start, *control1, *control2, *end),
                    color: *color,
                    thickness: *thickness,
                    closed: false,
                }),
                PaintOp::ParticleBatch { particles } => {
                    commands.push(FrameCommand::ParticleBatch {
                        particles: particles.clone(),
                    })
                }
                PaintOp::Text {
                    rect,
                    clip_rect,
                    text,
                    style,
                } => {
                    let render_style = RenderTextStyle {
                        color: style.color,
                        font_size: style.font_size,
                        horizontal_align: match style.horizontal_align {
                            ui_core::HorizontalAlign::Left => {
                                loadngo_host_core::RenderTextHorizontalAlign::Left
                            }
                            ui_core::HorizontalAlign::Center => {
                                loadngo_host_core::RenderTextHorizontalAlign::Center
                            }
                            ui_core::HorizontalAlign::Right => {
                                loadngo_host_core::RenderTextHorizontalAlign::Right
                            }
                        },
                        vertical_align: match style.vertical_align {
                            ui_core::VerticalAlign::Top => {
                                loadngo_host_core::RenderTextVerticalAlign::Top
                            }
                            ui_core::VerticalAlign::Middle => {
                                loadngo_host_core::RenderTextVerticalAlign::Middle
                            }
                            ui_core::VerticalAlign::Bottom => {
                                loadngo_host_core::RenderTextVerticalAlign::Bottom
                            }
                        },
                        vertical_metric_mode: match style.vertical_metric_mode {
                            ui_core::TextVerticalMetricMode::VisibleInk => {
                                loadngo_host_core::RenderTextVerticalMetricMode::VisibleInk
                            }
                            ui_core::TextVerticalMetricMode::LogicalLineBox => {
                                loadngo_host_core::RenderTextVerticalMetricMode::LogicalLineBox
                            }
                        },
                        layout_mode: match style.layout_mode {
                            ui_core::TextLayoutMode::SingleLine => {
                                loadngo_host_core::RenderTextLayoutMode::SingleLine
                            }
                            ui_core::TextLayoutMode::MultiLine => {
                                loadngo_host_core::RenderTextLayoutMode::MultiLine
                            }
                        },
                        overflow: match style.overflow {
                            ui_core::TextOverflow::Clip => {
                                loadngo_host_core::RenderTextOverflow::Clip
                            }
                            ui_core::TextOverflow::EllipsisEnd => {
                                loadngo_host_core::RenderTextOverflow::EllipsisEnd
                            }
                            ui_core::TextOverflow::EllipsisMiddle => {
                                loadngo_host_core::RenderTextOverflow::EllipsisMiddle
                            }
                        },
                    };
                    commands.push(FrameCommand::Text(self.text_request(
                        *rect,
                        *clip_rect,
                        text.clone(),
                        render_style,
                    )));
                }
                PaintOp::BlitImage { rect, image_key } => {
                    commands.push(FrameCommand::Image(ImageRequest {
                        rect: *rect,
                        clip_rect: None,
                        image_key: image_key.clone(),
                        alpha: 1.0,
                    }))
                }
            }
        }
        commands
    }

    pub fn render<B: GraphicsBackend>(
        &self,
        backend: &mut B,
        commands: &[FrameCommand],
    ) -> Result<(), RendererError> {
        backend.begin_frame()?;
        backend.submit(commands)?;
        backend.end_frame()
    }

    pub fn plan_frame_resources(&self, commands: &[FrameCommand]) -> FrameResourcePlan {
        let mut image_keys = Vec::new();
        for command in commands {
            if let FrameCommand::Image(request) = command {
                let key = ImageResourceKey::new(request.image_key.clone());
                if !image_keys.iter().any(|existing| existing == &key) {
                    image_keys.push(key);
                }
            }
        }
        FrameResourcePlan { image_keys }
    }

    fn text_request(
        &self,
        rect: Rect,
        clip_rect: Option<Rect>,
        text: String,
        style: RenderTextStyle,
    ) -> TextRequest {
        TextRequest {
            rect,
            clip_rect,
            text,
            style,
            font_source: None,
            direction: self.config.default_direction,
            script: self.config.default_script,
            language: self.config.default_language.clone(),
        }
    }
}

fn approximate_arc_points(
    center: Point,
    radius: f32,
    start_angle: f32,
    sweep_angle: f32,
) -> Vec<Point> {
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
            Point {
                x: center.x + radius * angle.cos(),
                y: center.y + radius * angle.sin(),
            }
        })
        .collect()
}

fn approximate_quadratic_points(start: Point, control: Point, end: Point) -> Vec<Point> {
    let segment_count = ((point_distance(start, control) + point_distance(control, end)) / 12.0)
        .ceil()
        .clamp(8.0, 64.0) as usize;
    (0..=segment_count)
        .map(|index| {
            let t = index as f32 / segment_count as f32;
            let one_minus_t = 1.0 - t;
            Point {
                x: one_minus_t * one_minus_t * start.x
                    + 2.0 * one_minus_t * t * control.x
                    + t * t * end.x,
                y: one_minus_t * one_minus_t * start.y
                    + 2.0 * one_minus_t * t * control.y
                    + t * t * end.y,
            }
        })
        .collect()
}

fn approximate_cubic_points(
    start: Point,
    control1: Point,
    control2: Point,
    end: Point,
) -> Vec<Point> {
    let segment_count = ((point_distance(start, control1)
        + point_distance(control1, control2)
        + point_distance(control2, end))
        / 12.0)
        .ceil()
        .clamp(10.0, 96.0) as usize;
    (0..=segment_count)
        .map(|index| {
            let t = index as f32 / segment_count as f32;
            let one_minus_t = 1.0 - t;
            Point {
                x: one_minus_t.powi(3) * start.x
                    + 3.0 * one_minus_t.powi(2) * t * control1.x
                    + 3.0 * one_minus_t * t * t * control2.x
                    + t.powi(3) * end.x,
                y: one_minus_t.powi(3) * start.y
                    + 3.0 * one_minus_t.powi(2) * t * control1.y
                    + 3.0 * one_minus_t * t * t * control2.y
                    + t.powi(3) * end.y,
            }
        })
        .collect()
}

fn point_distance(a: Point, b: Point) -> f32 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    (dx * dx + dy * dy).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Default)]
    struct RecordingBackend {
        events: Vec<String>,
        commands: Vec<FrameCommand>,
    }

    impl GraphicsBackend for RecordingBackend {
        fn begin_frame(&mut self) -> Result<(), RendererError> {
            self.events.push("begin".to_string());
            Ok(())
        }

        fn submit(&mut self, commands: &[FrameCommand]) -> Result<(), RendererError> {
            self.events.push("submit".to_string());
            self.commands.extend_from_slice(commands);
            Ok(())
        }

        fn end_frame(&mut self) -> Result<(), RendererError> {
            self.events.push("end".to_string());
            Ok(())
        }
    }

    #[test]
    fn paint_ops_encode_image_commands() {
        let renderer = Renderer::new(RendererConfig::default());
        let commands = renderer.encode_paint_ops(&[PaintOp::BlitImage {
            rect: Rect {
                x: 10.0,
                y: 20.0,
                width: 30.0,
                height: 40.0,
            },
            image_key: "scene/title.png".to_string(),
        }]);
        assert_eq!(
            commands,
            vec![FrameCommand::Image(ImageRequest {
                rect: Rect {
                    x: 10.0,
                    y: 20.0,
                    width: 30.0,
                    height: 40.0,
                },
                clip_rect: None,
                image_key: "scene/title.png".to_string(),
                alpha: 1.0,
            })]
        );
    }

    #[test]
    fn render_ops_preserve_text_metadata_defaults() {
        let renderer = Renderer::new(RendererConfig {
            default_language: Some(LanguageTag::new("ja-JP")),
            default_direction: TextDirection::LeftToRight,
            default_script: TextScript::Han,
            ..RendererConfig::default()
        });
        let commands = renderer.encode_render_ops(&[RenderOp::Text {
            rect: Rect {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 40.0,
            },
            text: "銀と金".to_string(),
            style: RenderTextStyle {
                color: Color::rgba(255, 255, 255, 255),
                font_size: 24,
                horizontal_align: loadngo_host_core::RenderTextHorizontalAlign::Center,
                vertical_align: loadngo_host_core::RenderTextVerticalAlign::Middle,
                vertical_metric_mode:
                    loadngo_host_core::RenderTextVerticalMetricMode::LogicalLineBox,
                layout_mode: loadngo_host_core::RenderTextLayoutMode::SingleLine,
                overflow: loadngo_host_core::RenderTextOverflow::Clip,
            },
        }]);
        match &commands[0] {
            FrameCommand::Text(request) => {
                assert_eq!(
                    request.language.as_ref().map(LanguageTag::as_str),
                    Some("ja-JP")
                );
                assert_eq!(request.direction, TextDirection::LeftToRight);
                assert_eq!(request.script, TextScript::Han);
            }
            other => panic!("expected text request, got {other:?}"),
        }
    }

    #[test]
    fn encode_paint_ops_preserves_text_alignment_and_overflow() {
        let renderer = Renderer::new(RendererConfig::default());
        let commands = renderer.encode_paint_ops(&[PaintOp::Text {
            rect: Rect {
                x: 4.0,
                y: 6.0,
                width: 120.0,
                height: 28.0,
            },
            clip_rect: Some(Rect {
                x: 10.0,
                y: 12.0,
                width: 80.0,
                height: 20.0,
            }),
            text: "preview".to_string(),
            style: ui_core::TextStyle {
                color: Color::rgba(255, 255, 255, 255),
                font_size: 18,
                horizontal_align: ui_core::HorizontalAlign::Right,
                vertical_align: ui_core::VerticalAlign::Bottom,
                vertical_metric_mode: ui_core::TextVerticalMetricMode::LogicalLineBox,
                layout_mode: ui_core::TextLayoutMode::SingleLine,
                overflow: ui_core::TextOverflow::EllipsisMiddle,
            },
        }]);
        match &commands[0] {
            FrameCommand::Text(request) => {
                assert_eq!(
                    request.style.horizontal_align,
                    loadngo_host_core::RenderTextHorizontalAlign::Right
                );
                assert_eq!(
                    request.style.vertical_align,
                    loadngo_host_core::RenderTextVerticalAlign::Bottom
                );
                assert_eq!(
                    request.style.layout_mode,
                    loadngo_host_core::RenderTextLayoutMode::SingleLine
                );
                assert_eq!(
                    request.style.overflow,
                    loadngo_host_core::RenderTextOverflow::EllipsisMiddle
                );
                assert_eq!(
                    request.clip_rect,
                    Some(Rect {
                        x: 10.0,
                        y: 12.0,
                        width: 80.0,
                        height: 20.0
                    })
                );
            }
            other => panic!("expected text request, got {other:?}"),
        }
    }

    #[test]
    fn encode_paint_ops_lowers_richer_shapes_into_frame_commands() {
        let renderer = Renderer::new(RendererConfig::default());
        let commands = renderer.encode_paint_ops(&[
            PaintOp::FillCircle {
                center: Point { x: 10.0, y: 12.0 },
                radius: 3.5,
                color: Color::rgba(1, 2, 3, 255),
            },
            PaintOp::ParticleBatch {
                particles: vec![ui_core::Particle {
                    center: Point { x: 20.0, y: 22.0 },
                    radius: 2.0,
                    color: Color::rgba(4, 5, 6, 200),
                }],
            },
            PaintOp::Polyline {
                points: vec![
                    Point { x: 0.0, y: 0.0 },
                    Point { x: 8.0, y: 0.0 },
                    Point { x: 8.0, y: 8.0 },
                ],
                color: Color::rgba(7, 8, 9, 255),
                thickness: 3,
                closed: false,
            },
            PaintOp::Arc {
                center: Point { x: 32.0, y: 32.0 },
                radius: 10.0,
                start_angle: 0.0,
                sweep_angle: std::f32::consts::PI * 0.75,
                color: Color::rgba(10, 11, 12, 255),
                thickness: 2,
            },
            PaintOp::QuadraticBezier {
                start: Point { x: 2.0, y: 20.0 },
                control: Point { x: 8.0, y: 12.0 },
                end: Point { x: 14.0, y: 20.0 },
                color: Color::rgba(13, 14, 15, 255),
                thickness: 2,
            },
            PaintOp::CubicBezier {
                start: Point { x: 2.0, y: 30.0 },
                control1: Point { x: 6.0, y: 22.0 },
                control2: Point { x: 10.0, y: 38.0 },
                end: Point { x: 14.0, y: 30.0 },
                color: Color::rgba(16, 17, 18, 255),
                thickness: 2,
            },
            PaintOp::StrokeCircle {
                center: Point { x: 44.0, y: 18.0 },
                radius: 6.0,
                color: Color::rgba(20, 21, 22, 255),
                thickness: 2,
            },
        ]);

        assert!(matches!(
            commands[0],
            FrameCommand::Circle {
                center: Point { x: 10.0, y: 12.0 },
                radius: 4.0,
                color: Color {
                    r: 1,
                    g: 2,
                    b: 3,
                    a: 255
                },
            }
        ));
        assert!(matches!(commands[1], FrameCommand::ParticleBatch { .. }));
        assert!(commands
            .iter()
            .any(|command| matches!(command, FrameCommand::Arc { .. })));
        assert!(commands
            .iter()
            .any(|command| matches!(command, FrameCommand::Polyline { .. })));
    }

    #[test]
    fn renderer_owns_frame_submission_order() {
        let renderer = Renderer::new(RendererConfig::default());
        let commands = renderer.encode_render_ops(&[RenderOp::Clear {
            color: Color::rgba(1, 2, 3, 255),
        }]);
        let mut backend = RecordingBackend::default();
        renderer
            .render(&mut backend, &commands)
            .expect("render should succeed");
        assert_eq!(backend.events, vec!["begin", "submit", "end"]);
        assert_eq!(backend.commands, commands);
    }

    #[test]
    fn renderer_plans_unique_image_resources() {
        let renderer = Renderer::new(RendererConfig::default());
        let plan = renderer.plan_frame_resources(&[
            FrameCommand::Image(ImageRequest {
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 10.0,
                },
                clip_rect: None,
                image_key: "scene/a.png".to_string(),
                alpha: 1.0,
            }),
            FrameCommand::Image(ImageRequest {
                rect: Rect {
                    x: 20.0,
                    y: 20.0,
                    width: 10.0,
                    height: 10.0,
                },
                clip_rect: None,
                image_key: "scene/a.png".to_string(),
                alpha: 0.5,
            }),
            FrameCommand::Image(ImageRequest {
                rect: Rect {
                    x: 30.0,
                    y: 30.0,
                    width: 10.0,
                    height: 10.0,
                },
                clip_rect: None,
                image_key: "scene/b.png".to_string(),
                alpha: 1.0,
            }),
        ]);
        assert_eq!(
            plan.image_keys,
            vec![
                ImageResourceKey::new("scene/a.png"),
                ImageResourceKey::new("scene/b.png"),
            ]
        );
    }

    #[test]
    fn font_catalog_prefers_shared_asset_font_paths() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should move forward")
            .as_nanos();
        let assets_root = std::env::temp_dir().join(format!("loadngo-font-assets-{unique}"));
        let font_path = assets_root.join("fonts").join("novel").join("Novel.otf");
        fs::create_dir_all(font_path.parent().expect("font parent should exist"))
            .expect("font dir should be created");
        fs::write(&font_path, b"font").expect("font file should be written");

        let manifest = FontCatalogManifest {
            novel_font: FontFaceManifest {
                asset_rel_paths: vec!["novel/Novel.otf".to_string()],
                platform_paths: PlatformFontPaths {
                    macos: vec!["/missing/system-font.otf".to_string()],
                    ..PlatformFontPaths::default()
                },
                ..FontFaceManifest::default()
            },
            ..FontCatalogManifest::default()
        };

        let resolved = manifest
            .resolve_novel_font_path(&assets_root, RuntimePlatform::MacOs)
            .expect("shared asset font should resolve");
        assert_eq!(resolved, font_path);

        fs::remove_dir_all(&assets_root).expect("temp assets should be removed");
    }

    #[test]
    fn font_catalog_can_fall_back_to_platform_font_paths() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should move forward")
            .as_nanos();
        let platform_font_path =
            std::env::temp_dir().join(format!("loadngo-platform-font-{unique}.otf"));
        fs::write(&platform_font_path, b"font").expect("platform font file should be written");

        let manifest = FontCatalogManifest {
            novel_font: FontFaceManifest {
                platform_paths: PlatformFontPaths {
                    macos: vec![platform_font_path.to_string_lossy().into_owned()],
                    ..PlatformFontPaths::default()
                },
                ..FontFaceManifest::default()
            },
            ..FontCatalogManifest::default()
        };

        let resolved = manifest
            .resolve_novel_font_path(Path::new("/missing-assets-root"), RuntimePlatform::MacOs)
            .expect("platform font path should resolve");
        assert_eq!(resolved, platform_font_path);

        fs::remove_file(&platform_font_path).expect("temp platform font should be removed");
    }
}
