use serde::{Deserialize, Serialize};

use crate::geometry::{Color, Point, Rect, Scalar};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HorizontalAlign {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerticalAlign {
    Top,
    Middle,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextLayoutMode {
    SingleLine,
    MultiLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextOverflow {
    Clip,
    EllipsisEnd,
    EllipsisMiddle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextVerticalMetricMode {
    VisibleInk,
    LogicalLineBox,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextStyle {
    pub color: Color,
    pub font_size: u16,
    pub horizontal_align: HorizontalAlign,
    pub vertical_align: VerticalAlign,
    pub vertical_metric_mode: TextVerticalMetricMode,
    pub layout_mode: TextLayoutMode,
    pub overflow: TextOverflow,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            color: Color::rgba(0x20, 0x20, 0x20, 0xff),
            font_size: 18,
            horizontal_align: HorizontalAlign::Left,
            vertical_align: VerticalAlign::Top,
            vertical_metric_mode: TextVerticalMetricMode::LogicalLineBox,
            layout_mode: TextLayoutMode::SingleLine,
            overflow: TextOverflow::Clip,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Particle {
    pub center: Point,
    pub radius: Scalar,
    pub color: Color,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PaintOp {
    FillRect {
        rect: Rect,
        color: Color,
    },
    StrokeRect {
        rect: Rect,
        color: Color,
    },
    Line {
        from: Point,
        to: Point,
        color: Color,
    },
    FillCircle {
        center: Point,
        radius: Scalar,
        color: Color,
    },
    StrokeCircle {
        center: Point,
        radius: Scalar,
        color: Color,
        thickness: i32,
    },
    Polyline {
        points: Vec<Point>,
        color: Color,
        thickness: i32,
        closed: bool,
    },
    Arc {
        center: Point,
        radius: Scalar,
        start_angle: Scalar,
        sweep_angle: Scalar,
        color: Color,
        thickness: i32,
    },
    QuadraticBezier {
        start: Point,
        control: Point,
        end: Point,
        color: Color,
        thickness: i32,
    },
    CubicBezier {
        start: Point,
        control1: Point,
        control2: Point,
        end: Point,
        color: Color,
        thickness: i32,
    },
    ParticleBatch {
        particles: Vec<Particle>,
    },
    Text {
        rect: Rect,
        clip_rect: Option<Rect>,
        text: String,
        style: TextStyle,
    },
    BlitImage {
        rect: Rect,
        image_key: String,
    },
}
