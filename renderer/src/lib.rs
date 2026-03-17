use std::path::{Path, PathBuf};

use loadngo_host_core::{RenderOp, RenderTextStyle};
use serde::{Deserialize, Serialize};
use ui_core::{
    geometry::{Color, Point, Rect},
    paint::PaintOp,
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

    pub fn candidate_paths(
        &self,
        assets_root: &Path,
        platform: RuntimePlatform,
    ) -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let prefer_platform_fonts = matches!(platform, RuntimePlatform::Android | RuntimePlatform::Ios);

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
        self.candidate_paths(assets_root, platform).into_iter().next()
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextRequest {
    pub rect: Rect,
    pub text: String,
    pub style: RenderTextStyle,
    pub direction: TextDirection,
    pub script: TextScript,
    pub language: Option<LanguageTag>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageRequest {
    pub rect: Rect,
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
        radius: i32,
        color: Color,
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
                    FrameCommand::Text(self.text_request(*rect, text.clone(), style.clone()))
                }
                RenderOp::BlitImage {
                    rect,
                    image_key,
                    alpha,
                } => FrameCommand::Image(ImageRequest {
                    rect: *rect,
                    image_key: image_key.clone(),
                    alpha: *alpha,
                }),
            })
            .collect()
    }

    pub fn encode_paint_ops(&self, ops: &[PaintOp]) -> Vec<FrameCommand> {
        ops.iter()
            .map(|op| match op {
                PaintOp::FillRect { rect, color } => FrameCommand::FillRect {
                    rect: *rect,
                    color: *color,
                },
                PaintOp::StrokeRect { rect, color } => FrameCommand::StrokeRect {
                    rect: *rect,
                    color: *color,
                    thickness: self.config.widget_stroke_thickness,
                },
                PaintOp::Line { from, to, color } => FrameCommand::Line {
                    from: *from,
                    to: *to,
                    color: *color,
                    thickness: self.config.widget_stroke_thickness,
                },
                PaintOp::Text { rect, text, style } => {
                    let render_style = RenderTextStyle {
                        color: style.color,
                        font_size: self.config.widget_font_size,
                        centered: style.centered,
                    };
                    FrameCommand::Text(self.text_request(*rect, text.clone(), render_style))
                }
                PaintOp::BlitImage { rect, image_key } => FrameCommand::Image(ImageRequest {
                    rect: *rect,
                    image_key: image_key.clone(),
                    alpha: 1.0,
                }),
            })
            .collect()
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

    fn text_request(&self, rect: Rect, text: String, style: RenderTextStyle) -> TextRequest {
        TextRequest {
            rect,
            text,
            style,
            direction: self.config.default_direction,
            script: self.config.default_script,
            language: self.config.default_language.clone(),
        }
    }
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
                x: 10,
                y: 20,
                width: 30,
                height: 40,
            },
            image_key: "scene/title.png".to_string(),
        }]);
        assert_eq!(
            commands,
            vec![FrameCommand::Image(ImageRequest {
                rect: Rect {
                    x: 10,
                    y: 20,
                    width: 30,
                    height: 40,
                },
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
                x: 0,
                y: 0,
                width: 200,
                height: 40,
            },
            text: "銀と金".to_string(),
            style: RenderTextStyle {
                color: Color::rgba(255, 255, 255, 255),
                font_size: 24,
                centered: true,
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
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                },
                image_key: "scene/a.png".to_string(),
                alpha: 1.0,
            }),
            FrameCommand::Image(ImageRequest {
                rect: Rect {
                    x: 20,
                    y: 20,
                    width: 10,
                    height: 10,
                },
                image_key: "scene/a.png".to_string(),
                alpha: 0.5,
            }),
            FrameCommand::Image(ImageRequest {
                rect: Rect {
                    x: 30,
                    y: 30,
                    width: 10,
                    height: 10,
                },
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
