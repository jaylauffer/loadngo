use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use image::DynamicImage;
use serde::{Deserialize, Serialize};
use ui_core::{
    geometry::{Color, Point, Rect},
    Modifiers,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowDescriptor {
    pub title: String,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub high_dpi: bool,
    pub linux_wm_class: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

impl DecodedImage {
    pub fn new(width: u32, height: u32, rgba8: Vec<u8>) -> Self {
        Self {
            width,
            height,
            rgba8,
        }
    }

    pub fn expected_len(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    pub fn validate_rgba8(&self) -> Result<(), String> {
        let expected = self.expected_len();
        let actual = self.rgba8.len();
        if actual != expected {
            return Err(format!(
                "decoded image buffer length mismatch: expected {expected} bytes, got {actual}"
            ));
        }
        Ok(())
    }
}

pub fn decode_image_from_memory(bytes: &[u8]) -> Result<DecodedImage, String> {
    let decoded =
        image::load_from_memory(bytes).map_err(|err| format!("unsupported image format: {err}"))?;
    decoded_image_from_dynamic(decoded)
}

pub fn decode_image_from_path(path: &Path) -> Result<DecodedImage, String> {
    let decoded = image::open(path)
        .map_err(|err| format!("failed to decode image ({}): {err}", path.display()))?;
    decoded_image_from_dynamic(decoded)
}

fn decoded_image_from_dynamic(decoded: DynamicImage) -> Result<DecodedImage, String> {
    let rgba = decoded.to_rgba8();
    let (width, height) = rgba.dimensions();
    let image = DecodedImage::new(width, height, rgba.into_raw());
    image.validate_rgba8()?;
    Ok(image)
}

#[derive(Debug, Clone, Default)]
pub struct ImageRegistry {
    images: HashMap<String, DecodedImage>,
}

impl ImageRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, image_key: impl Into<String>, image: DecodedImage) {
        self.images.insert(image_key.into(), image);
    }

    pub fn get(&self, image_key: &str) -> Option<&DecodedImage> {
        self.images.get(image_key)
    }

    pub fn load_path(
        &mut self,
        image_key: impl Into<String>,
        path: &Path,
    ) -> Result<&DecodedImage, String> {
        let image_key = image_key.into();
        if !self.images.contains_key(&image_key) {
            let decoded = decode_image_from_path(path)?;
            self.images.insert(image_key.clone(), decoded);
        }
        self.images
            .get(&image_key)
            .ok_or_else(|| format!("missing image after load: {image_key}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowIconSet {
    pub small_rgba8: Vec<u8>,
    pub medium_rgba8: Vec<u8>,
    pub big_rgba8: Vec<u8>,
}

impl WindowIconSet {
    pub fn from_source_rgba(source: &[u8], src_w: usize, src_h: usize) -> Self {
        Self {
            small_rgba8: resize_rgba(source, src_w, src_h, 16),
            medium_rgba8: resize_rgba(source, src_w, src_h, 32),
            big_rgba8: resize_rgba(source, src_w, src_h, 64),
        }
    }
}

pub fn resize_rgba(source: &[u8], src_w: usize, src_h: usize, target: usize) -> Vec<u8> {
    let mut resized = vec![0u8; target * target * 4];
    for y in 0..target {
        let src_y = ((y * src_h) / target).min(src_h.saturating_sub(1));
        for x in 0..target {
            let src_x = ((x * src_w) / target).min(src_w.saturating_sub(1));
            let src_idx = (src_y * src_w + src_x) * 4;
            let dst_idx = (y * target + x) * 4;
            resized[dst_idx..dst_idx + 4].copy_from_slice(&source[src_idx..src_idx + 4]);
        }
    }
    resized
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FrameTiming {
    pub delta_seconds: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameDemand {
    Idle,
    After(Duration),
}

impl FrameDemand {
    pub const fn idle() -> Self {
        Self::Idle
    }

    pub fn after(duration: Duration) -> Self {
        Self::After(duration)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SurfaceInfo {
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct PointF {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl RectF {
    pub fn contains(self, point: PointF) -> bool {
        point.x >= self.x
            && point.x < self.x + self.width
            && point.y >= self.y
            && point.y < self.y + self.height
    }

    pub fn right(self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(self) -> f32 {
        self.y + self.height
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextMetrics {
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextBlockStyle {
    pub color: Color,
    pub font_size: u16,
    pub font_scale: f32,
    pub line_spacing: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostFrame {
    pub timing: FrameTiming,
    pub surface: SurfaceInfo,
    pub input: InputSnapshot,
    /// Whether the host process is currently in the foreground (visible and
    /// interactive) according to the platform's own lifecycle signal. On
    /// Android this reflects `onResume`/`onPause` (screen lock, task switch,
    /// an incoming call, etc. all report `false` here); platforms with no
    /// such background/foreground distinction yet wired up always report
    /// `true`. Games should stop advancing simulation time and pause audio
    /// while this is `false`, matching platform-expected app-lifecycle
    /// behavior instead of continuing to run (and make noise) off-screen.
    pub foreground: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostKey {
    Escape,
    Space,
    F3,
    R,
    C,
    S,
    V,
    Y,
    Z,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Enter,
    Tab,
    Backspace,
    Delete,
    A,
    D,
    F,
    W,
    T,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostKeyEvent {
    pub key: HostKey,
    pub modifiers: Modifiers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TouchPhase {
    Started,
    Moved,
    Stationary,
    Ended,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TouchPoint {
    pub id: u64,
    pub x: f32,
    pub y: f32,
    pub phase: TouchPhase,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputSnapshot {
    pub mouse_x: f32,
    pub mouse_y: f32,
    pub mouse_wheel_x: f32,
    pub mouse_wheel_y: f32,
    pub mouse_pressed: bool,
    pub mouse_down: bool,
    pub mouse_released: bool,
    pub touches: [Option<TouchPoint>; 8],
    pub escape_pressed: bool,
    pub space_pressed: bool,
    pub space_down: bool,
    pub f3_pressed: bool,
    pub r_pressed: bool,
    pub up_pressed: bool,
    pub down_pressed: bool,
    pub modifiers: Modifiers,
    pub key_events: Vec<HostKeyEvent>,
    #[serde(default)]
    pub keys_down: Vec<HostKey>,
    pub typed_text: String,
}

impl InputSnapshot {
    /// Discards keyboard state when the host loses input focus.
    ///
    /// Pointer and touch state are intentionally left unchanged because their
    /// lifecycle is reported independently by the host backend.
    pub fn clear_keyboard_state(&mut self) {
        self.escape_pressed = false;
        self.space_pressed = false;
        self.space_down = false;
        self.f3_pressed = false;
        self.r_pressed = false;
        self.up_pressed = false;
        self.down_pressed = false;
        self.modifiers = Modifiers::default();
        self.key_events.clear();
        self.keys_down.clear();
        self.typed_text.clear();
    }

    pub fn key_pressed(&self, key: HostKey) -> bool {
        match key {
            HostKey::Escape => self.escape_pressed,
            HostKey::Space => self.space_pressed,
            HostKey::F3 => self.f3_pressed,
            HostKey::R => self.r_pressed,
            HostKey::Up => self.up_pressed,
            HostKey::Down => self.down_pressed,
            _ => self.key_events.iter().any(|event| event.key == key),
        }
    }

    pub fn key_down(&self, key: HostKey) -> bool {
        (key == HostKey::Space && self.space_down) || self.keys_down.contains(&key)
    }

    pub fn active_touches(&self) -> impl Iterator<Item = TouchPoint> + '_ {
        self.touches.iter().flatten().copied()
    }

    pub fn mouse_point(&self) -> PointF {
        PointF {
            x: self.mouse_x,
            y: self.mouse_y,
        }
    }

    pub fn mouse_in_rect(&self, rect: RectF) -> bool {
        rect.contains(self.mouse_point())
    }

    pub fn touch_in_rect(&self, rect: RectF) -> bool {
        self.active_touches().any(|touch| {
            rect.contains(PointF {
                x: touch.x,
                y: touch.y,
            })
        })
    }

    pub fn pointer_in_rect(&self, rect: RectF) -> bool {
        self.mouse_in_rect(rect) || self.touch_in_rect(rect)
    }

    pub fn pointer_pressed_in_rect(&self, rect: RectF) -> bool {
        (self.mouse_pressed && self.mouse_in_rect(rect))
            || self.active_touches().any(|touch| {
                touch.phase == TouchPhase::Started
                    && rect.contains(PointF {
                        x: touch.x,
                        y: touch.y,
                    })
            })
    }

    pub fn pointer_released(&self) -> bool {
        self.mouse_released
            || self
                .active_touches()
                .any(|touch| matches!(touch.phase, TouchPhase::Ended | TouchPhase::Cancelled))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RenderTextHorizontalAlign {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RenderTextVerticalAlign {
    Top,
    Middle,
    Bottom,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RenderTextLayoutMode {
    SingleLine,
    MultiLine,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RenderTextOverflow {
    Clip,
    EllipsisEnd,
    EllipsisMiddle,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RenderTextVerticalMetricMode {
    VisibleInk,
    LogicalLineBox,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderTextStyle {
    pub color: Color,
    pub font_size: u16,
    pub horizontal_align: RenderTextHorizontalAlign,
    pub vertical_align: RenderTextVerticalAlign,
    pub vertical_metric_mode: RenderTextVerticalMetricMode,
    pub layout_mode: RenderTextLayoutMode,
    pub overflow: RenderTextOverflow,
}

impl Default for RenderTextStyle {
    fn default() -> Self {
        Self {
            color: Color::rgba(0x20, 0x20, 0x20, 0xff),
            font_size: 18,
            horizontal_align: RenderTextHorizontalAlign::Left,
            vertical_align: RenderTextVerticalAlign::Top,
            vertical_metric_mode: RenderTextVerticalMetricMode::LogicalLineBox,
            layout_mode: RenderTextLayoutMode::SingleLine,
            overflow: RenderTextOverflow::Clip,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RenderOp {
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
    Text {
        rect: Rect,
        text: String,
        style: RenderTextStyle,
    },
    BlitImage {
        rect: Rect,
        image_key: String,
        alpha: f32,
    },
}

pub trait DesktopPlatformBackend {
    fn launch<F>(window: WindowDescriptor, icon: Option<WindowIconSet>, entry: F)
    where
        F: Future<Output = ()> + 'static;

    fn capture_frame() -> HostFrame;

    fn next_frame(demand: FrameDemand) -> Pin<Box<dyn Future<Output = ()>>>;

    fn simulate_mouse_with_touch(enabled: bool);
}

pub trait AssetIoBackend {
    fn load_bytes(path: &str) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>>>>;

    fn load_text(path: &str) -> Pin<Box<dyn Future<Output = Result<String, String>>>>;
}

pub trait DesktopGraphicsBackend {
    type FontHandle;
    type TextureHandle: Clone;

    fn load_font(path: &str) -> Pin<Box<dyn Future<Output = Result<Self::FontHandle, String>>>>;

    fn measure_text(
        text: &str,
        font: Option<&Self::FontHandle>,
        font_size: u16,
        font_scale: f32,
    ) -> TextMetrics;

    fn render_ops(ops: &[RenderOp], font: Option<&Self::FontHandle>);

    fn upload_texture(image: &DecodedImage) -> Result<Self::TextureHandle, String>;

    fn blit_texture(texture: &Self::TextureHandle, rect: Rect, alpha: f32);
}

pub trait DesktopHostBackend:
    DesktopPlatformBackend + AssetIoBackend + DesktopGraphicsBackend
{
}

impl<T> DesktopHostBackend for T where
    T: DesktopPlatformBackend + AssetIoBackend + DesktopGraphicsBackend
{
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngEncoder;
    use image::{ColorType, ImageEncoder};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn rgba_test_pixels() -> (u32, u32, Vec<u8>) {
        (
            2,
            1,
            vec![
                255, 0, 0, 255, //
                0, 255, 0, 255,
            ],
        )
    }

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
            modifiers: Modifiers::default(),
            key_events: Vec::new(),
            keys_down: Vec::new(),
            typed_text: String::new(),
        }
    }

    fn encode_png_fixture() -> Vec<u8> {
        let (width, height, rgba8) = rgba_test_pixels();
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&rgba8, width, height, ColorType::Rgba8.into())
            .expect("png fixture encoding should succeed");
        bytes
    }

    fn encode_jpeg_fixture() -> Vec<u8> {
        let rgb8 = vec![
            255, 0, 0, //
            0, 255, 0,
        ];
        let mut bytes = Vec::new();
        JpegEncoder::new(&mut bytes)
            .encode(&rgb8, 2, 1, ColorType::Rgb8.into())
            .expect("jpeg fixture encoding should succeed");
        bytes
    }

    #[test]
    fn resize_rgba_preserves_single_source_pixel() {
        let src = vec![10, 20, 30, 40];
        let resized = resize_rgba(&src, 1, 1, 2);
        assert_eq!(resized.len(), 16);
        for chunk in resized.chunks_exact(4) {
            assert_eq!(chunk, &[10, 20, 30, 40]);
        }
    }

    #[test]
    fn rectf_contains_uses_half_open_edges() {
        let rect = RectF {
            x: 10.0,
            y: 20.0,
            width: 30.0,
            height: 40.0,
        };

        assert!(rect.contains(PointF { x: 10.0, y: 20.0 }));
        assert!(rect.contains(PointF { x: 39.9, y: 59.9 }));
        assert!(!rect.contains(PointF { x: 40.0, y: 59.9 }));
        assert!(!rect.contains(PointF { x: 39.9, y: 60.0 }));
    }

    #[test]
    fn window_icon_set_produces_expected_sizes() {
        let src = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
        ];
        let icons = WindowIconSet::from_source_rgba(&src, 2, 2);
        assert_eq!(icons.small_rgba8.len(), 16 * 16 * 4);
        assert_eq!(icons.medium_rgba8.len(), 32 * 32 * 4);
        assert_eq!(icons.big_rgba8.len(), 64 * 64 * 4);
    }

    #[test]
    fn input_snapshot_reports_pressed_and_held_keys() {
        let snapshot = InputSnapshot {
            escape_pressed: true,
            space_down: true,
            keys_down: vec![HostKey::A, HostKey::Up],
            ..blank_snapshot()
        };

        assert!(snapshot.key_pressed(HostKey::Escape));
        assert!(!snapshot.key_pressed(HostKey::Space));
        assert!(snapshot.key_down(HostKey::Space));
        assert!(snapshot.key_down(HostKey::A));
        assert!(snapshot.key_down(HostKey::Up));
        assert!(!snapshot.key_down(HostKey::D));
    }

    #[test]
    fn input_snapshot_reports_event_backed_navigation_keys() {
        let snapshot = InputSnapshot {
            key_events: vec![HostKeyEvent {
                key: HostKey::Left,
                modifiers: Modifiers {
                    shift: true,
                    ctrl: false,
                    alt: false,
                    meta: false,
                },
            }],
            typed_text: "abc".to_string(),
            ..blank_snapshot()
        };

        assert!(snapshot.key_pressed(HostKey::Left));
        assert_eq!(snapshot.typed_text, "abc");
        assert!(snapshot.key_events[0].modifiers.shift);
    }

    #[test]
    fn input_snapshot_clears_keyboard_state_without_touching_pointer_state() {
        let mut snapshot = InputSnapshot {
            mouse_x: 12.0,
            mouse_y: 34.0,
            mouse_down: true,
            escape_pressed: true,
            space_pressed: true,
            space_down: true,
            f3_pressed: true,
            r_pressed: true,
            up_pressed: true,
            down_pressed: true,
            modifiers: Modifiers {
                shift: true,
                ctrl: true,
                alt: true,
                meta: true,
            },
            key_events: vec![HostKeyEvent {
                key: HostKey::D,
                modifiers: Modifiers::default(),
            }],
            keys_down: vec![HostKey::D, HostKey::Left, HostKey::Space],
            typed_text: "queued".to_string(),
            ..blank_snapshot()
        };

        snapshot.clear_keyboard_state();

        assert!(!snapshot.escape_pressed);
        assert!(!snapshot.space_pressed);
        assert!(!snapshot.space_down);
        assert!(!snapshot.f3_pressed);
        assert!(!snapshot.r_pressed);
        assert!(!snapshot.up_pressed);
        assert!(!snapshot.down_pressed);
        assert_eq!(snapshot.modifiers, Modifiers::default());
        assert!(snapshot.key_events.is_empty());
        assert!(snapshot.keys_down.is_empty());
        assert!(snapshot.typed_text.is_empty());
        assert_eq!(snapshot.mouse_x, 12.0);
        assert_eq!(snapshot.mouse_y, 34.0);
        assert!(snapshot.mouse_down);
    }

    #[test]
    fn input_snapshot_iterates_only_active_touches() {
        let snapshot = InputSnapshot {
            touches: [
                Some(TouchPoint {
                    id: 7,
                    x: 12.0,
                    y: 18.0,
                    phase: TouchPhase::Started,
                }),
                None,
                Some(TouchPoint {
                    id: 9,
                    x: 44.0,
                    y: 52.0,
                    phase: TouchPhase::Moved,
                }),
                None,
                None,
                None,
                None,
                None,
            ],
            ..blank_snapshot()
        };

        let ids: Vec<u64> = snapshot.active_touches().map(|touch| touch.id).collect();
        assert_eq!(ids, vec![7, 9]);
    }

    #[test]
    fn input_snapshot_pointer_hit_test_checks_mouse_and_touches() {
        let rect = RectF {
            x: 20.0,
            y: 20.0,
            width: 40.0,
            height: 30.0,
        };

        let mouse_hit = InputSnapshot {
            mouse_x: 30.0,
            mouse_y: 25.0,
            ..blank_snapshot()
        };
        assert!(mouse_hit.mouse_in_rect(rect));
        assert!(mouse_hit.pointer_in_rect(rect));

        let touch_hit = InputSnapshot {
            touches: [
                Some(TouchPoint {
                    id: 1,
                    x: 22.0,
                    y: 48.0,
                    phase: TouchPhase::Stationary,
                }),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ],
            ..blank_snapshot()
        };
        assert!(!touch_hit.mouse_in_rect(rect));
        assert!(touch_hit.touch_in_rect(rect));
        assert!(touch_hit.pointer_in_rect(rect));
    }

    #[test]
    fn input_snapshot_pointer_pressed_uses_mouse_or_touch_start() {
        let rect = RectF {
            x: 100.0,
            y: 100.0,
            width: 20.0,
            height: 20.0,
        };

        let mouse_press = InputSnapshot {
            mouse_x: 105.0,
            mouse_y: 108.0,
            mouse_pressed: true,
            ..blank_snapshot()
        };
        assert!(mouse_press.pointer_pressed_in_rect(rect));

        let touch_start_press = InputSnapshot {
            touches: [
                Some(TouchPoint {
                    id: 33,
                    x: 110.0,
                    y: 110.0,
                    phase: TouchPhase::Started,
                }),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ],
            ..blank_snapshot()
        };
        assert!(touch_start_press.pointer_pressed_in_rect(rect));

        let touch_move = InputSnapshot {
            touches: [
                Some(TouchPoint {
                    id: 33,
                    x: 110.0,
                    y: 110.0,
                    phase: TouchPhase::Moved,
                }),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ],
            ..blank_snapshot()
        };
        assert!(!touch_move.pointer_pressed_in_rect(rect));
    }

    #[test]
    fn input_snapshot_pointer_released_detects_touch_end_or_cancel() {
        let touch_end = InputSnapshot {
            touches: [
                Some(TouchPoint {
                    id: 3,
                    x: 1.0,
                    y: 1.0,
                    phase: TouchPhase::Ended,
                }),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ],
            ..blank_snapshot()
        };
        assert!(touch_end.pointer_released());

        let touch_cancel = InputSnapshot {
            touches: [
                Some(TouchPoint {
                    id: 4,
                    x: 1.0,
                    y: 1.0,
                    phase: TouchPhase::Cancelled,
                }),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ],
            ..blank_snapshot()
        };
        assert!(touch_cancel.pointer_released());
    }

    #[test]
    fn decoded_image_validates_rgba_length() {
        let valid = DecodedImage::new(2, 1, vec![0; 8]);
        assert_eq!(valid.validate_rgba8(), Ok(()));

        let invalid = DecodedImage::new(2, 1, vec![0; 7]);
        assert!(invalid.validate_rgba8().is_err());
    }

    #[test]
    fn decode_image_from_memory_rejects_invalid_bytes() {
        let err = decode_image_from_memory(b"not-an-image").unwrap_err();
        assert!(err.contains("unsupported image format"));
    }

    #[test]
    fn decode_image_from_memory_decodes_png_fixture() {
        let decoded = decode_image_from_memory(&encode_png_fixture())
            .expect("png fixture should decode successfully");

        assert_eq!(decoded.width, 2);
        assert_eq!(decoded.height, 1);
        assert_eq!(decoded.rgba8.len(), 8);
        assert_eq!(&decoded.rgba8[0..4], &[255, 0, 0, 255]);
        assert_eq!(&decoded.rgba8[4..8], &[0, 255, 0, 255]);
    }

    #[test]
    fn decode_image_from_memory_decodes_jpeg_fixture() {
        let decoded = decode_image_from_memory(&encode_jpeg_fixture())
            .expect("jpeg fixture should decode successfully");

        assert_eq!(decoded.width, 2);
        assert_eq!(decoded.height, 1);
        assert_eq!(decoded.rgba8.len(), 8);
        assert_eq!(decoded.rgba8[3], 255);
        assert_eq!(decoded.rgba8[7], 255);
    }

    #[test]
    fn decode_image_from_path_decodes_png_fixture() {
        let mut path = std::env::temp_dir();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        path.push(format!("loadngo-host-core-test-{unique}.png"));
        fs::write(&path, encode_png_fixture()).expect("fixture file write should succeed");

        let decoded = decode_image_from_path(&path).expect("png fixture path should decode");
        let _ = fs::remove_file(&path);

        assert_eq!(decoded.width, 2);
        assert_eq!(decoded.height, 1);
        assert_eq!(&decoded.rgba8[0..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn decode_image_from_path_reports_missing_file() {
        let mut path = std::env::temp_dir();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        path.push(format!("loadngo-host-core-missing-{unique}.png"));

        let err = decode_image_from_path(&path).unwrap_err();
        assert!(err.contains("failed to decode image"));
        assert!(err.contains(&path.display().to_string()));
    }

    #[test]
    fn image_registry_loads_and_returns_inserted_image() {
        let mut registry = ImageRegistry::new();
        let image = DecodedImage::new(1, 1, vec![1, 2, 3, 4]);
        registry.insert("logo", image.clone());
        assert_eq!(registry.get("logo"), Some(&image));
    }

    #[test]
    fn image_registry_insert_overwrites_existing_image() {
        let mut registry = ImageRegistry::new();
        registry.insert("logo", DecodedImage::new(1, 1, vec![1, 2, 3, 4]));
        registry.insert("logo", DecodedImage::new(1, 1, vec![9, 8, 7, 6]));

        let image = registry
            .get("logo")
            .expect("overwritten registry entry should be present");
        assert_eq!(image.rgba8, vec![9, 8, 7, 6]);
    }
}
