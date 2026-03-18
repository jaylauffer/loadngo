use serde::{Deserialize, Serialize};

use crate::geometry::{Color, Point, Rect};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextStyle {
    pub color: Color,
    pub font_size: u16,
    pub centered: bool,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            color: Color::rgba(0x20, 0x20, 0x20, 0xff),
            font_size: 18,
            centered: false,
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
