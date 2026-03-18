use serde::{Deserialize, Serialize};

use crate::geometry::{Color, Point, Rect};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextStyle {
    pub color: Color,
    pub font_size: u16,
    pub horizontal_align: HorizontalAlign,
    pub vertical_align: VerticalAlign,
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
            layout_mode: TextLayoutMode::SingleLine,
            overflow: TextOverflow::Clip,
        }
    }
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
    Text {
        rect: Rect,
        text: String,
        style: TextStyle,
    },
    BlitImage {
        rect: Rect,
        image_key: String,
    },
}
