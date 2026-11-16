//! Colours and small painting helpers: a dark panel with one accent.

use ui_core::{
    Color, HorizontalAlign, PaintOp, Point, Rect, TextLayoutMode, TextOverflow, TextStyle,
    TextVerticalMetricMode, VerticalAlign,
};

pub const BACKGROUND: Color = Color::rgba(0x17, 0x19, 0x1d, 0xff);
pub const HEADER: Color = Color::rgba(0x1f, 0x22, 0x27, 0xff);
pub const PANEL: Color = Color::rgba(0x24, 0x27, 0x2d, 0xff);
pub const PANEL_HOT: Color = Color::rgba(0x2b, 0x2f, 0x36, 0xff);
pub const PANEL_SELECTED: Color = Color::rgba(0x2a, 0x33, 0x44, 0xff);
pub const EDGE: Color = Color::rgba(0x38, 0x3d, 0x46, 0xff);
pub const TEXT: Color = Color::rgba(0xec, 0xed, 0xee, 0xff);
pub const DIM_TEXT: Color = Color::rgba(0x9a, 0xa0, 0xaa, 0xff);
pub const HEADING_TEXT: Color = Color::rgba(0x7d, 0x84, 0x90, 0xff);
pub const ACCENT: Color = Color::rgba(0x4f, 0x8f, 0xe8, 0xff);
pub const ACCENT_HOT: Color = Color::rgba(0x69, 0xa3, 0xf2, 0xff);
pub const ON_ACCENT: Color = Color::rgba(0xff, 0xff, 0xff, 0xff);
pub const BUTTON: Color = Color::rgba(0x33, 0x38, 0x41, 0xff);
pub const BUTTON_HOT: Color = Color::rgba(0x3f, 0x45, 0x50, 0xff);
pub const WARN: Color = Color::rgba(0xc2, 0x4b, 0x44, 0xff);
pub const WARN_HOT: Color = Color::rgba(0xd9, 0x5b, 0x53, 0xff);
pub const IDLE_DOT: Color = Color::rgba(0x55, 0x5b, 0x66, 0xff);
pub const BUSY_DOT: Color = Color::rgba(0xe2, 0xb3, 0x40, 0xff);
pub const UP_DOT: Color = Color::rgba(0x4c, 0xc3, 0x8a, 0xff);
pub const FAILED: Color = Color::rgba(0xf0, 0x6a, 0x62, 0xff);

pub const TITLE_FONT: u16 = 20;
pub const NAME_FONT: u16 = 16;
pub const FONT: u16 = 13;
pub const SMALL_FONT: u16 = 12;

pub fn fill(scene: &mut Vec<PaintOp>, rect: Rect, color: Color) {
    if rect.width > 0.0 && rect.height > 0.0 {
        scene.push(PaintOp::FillRect { rect, color });
    }
}

pub fn stroke(scene: &mut Vec<PaintOp>, rect: Rect, color: Color) {
    if rect.width > 0.0 && rect.height > 0.0 {
        scene.push(PaintOp::StrokeRect { rect, color });
    }
}

pub fn line(scene: &mut Vec<PaintOp>, from: Point, to: Point, color: Color) {
    scene.push(PaintOp::Line { from, to, color });
}

pub fn dot(scene: &mut Vec<PaintOp>, center: Point, radius: f32, color: Color) {
    scene.push(PaintOp::FillCircle {
        center,
        radius,
        color,
    });
}

/// One line of text in `rect`, vertically centred, ending in an ellipsis
/// where it does not fit.
pub fn text(
    scene: &mut Vec<PaintOp>,
    rect: Rect,
    text: impl Into<String>,
    font_size: u16,
    color: Color,
    align: HorizontalAlign,
) {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    scene.push(PaintOp::Text {
        rect,
        clip_rect: None,
        text: text.into(),
        style: TextStyle {
            color,
            font_size,
            horizontal_align: align,
            vertical_align: VerticalAlign::Middle,
            vertical_metric_mode: TextVerticalMetricMode::LogicalLineBox,
            layout_mode: TextLayoutMode::SingleLine,
            overflow: TextOverflow::EllipsisEnd,
        },
    });
}

/// How a button reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// The thing to press: start, open.
    Main,
    Plain,
    /// Stops something.
    Warn,
}

pub fn button(scene: &mut Vec<PaintOp>, rect: Rect, label: &str, tone: Tone, hot: bool) {
    let (back, ink) = match (tone, hot) {
        (Tone::Main, false) => (ACCENT, ON_ACCENT),
        (Tone::Main, true) => (ACCENT_HOT, ON_ACCENT),
        (Tone::Plain, false) => (BUTTON, TEXT),
        (Tone::Plain, true) => (BUTTON_HOT, TEXT),
        (Tone::Warn, false) => (WARN, ON_ACCENT),
        (Tone::Warn, true) => (WARN_HOT, ON_ACCENT),
    };
    fill(scene, rect, back);
    text(scene, rect, label, FONT, ink, HorizontalAlign::Center);
}
