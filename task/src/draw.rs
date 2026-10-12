//! Colours and small painting helpers. The look follows the Win32 Task: a
//! white planner on a grey gutter, blue planned entries, green actual ones.

use ui_core::{
    Color, HorizontalAlign, PaintOp, Point, Rect, TextLayoutMode, TextOverflow, TextStyle,
    TextVerticalMetricMode, VerticalAlign,
};

pub const BACKGROUND: Color = Color::rgba(0xf4, 0xf4, 0xf2, 0xff);
pub const PANEL: Color = Color::rgba(0xff, 0xff, 0xff, 0xff);
pub const TOOLBAR: Color = Color::rgba(0xe6, 0xe6, 0xe3, 0xff);
pub const BORDER: Color = Color::rgba(0xc4, 0xc4, 0xc0, 0xff);
pub const GRID: Color = Color::rgba(0xe8, 0xe8, 0xe6, 0xff);
pub const HOUR_LINE: Color = Color::rgba(0xc8, 0xc8, 0xc4, 0xff);
pub const GUTTER: Color = Color::rgba(0xe6, 0xe6, 0xe6, 0xff);
pub const TEXT: Color = Color::rgba(0x20, 0x20, 0x20, 0xff);
pub const DIM_TEXT: Color = Color::rgba(0x70, 0x70, 0x6c, 0xff);
pub const ERROR_TEXT: Color = Color::rgba(0xa8, 0x1e, 0x1e, 0xff);
pub const SELECTION: Color = Color::rgba(0xd4, 0xe2, 0xf6, 0xff);
pub const HOVER: Color = Color::rgba(0xec, 0xf1, 0xf9, 0xff);
pub const ACCENT: Color = Color::rgba(0x2f, 0x5f, 0xa8, 0xff);
pub const BUTTON: Color = Color::rgba(0xfb, 0xfb, 0xfa, 0xff);
pub const BUTTON_DISABLED_TEXT: Color = Color::rgba(0xa8, 0xa8, 0xa4, 0xff);
pub const DROP_TARGET: Color = Color::rgba(0xff, 0xe9, 0xb0, 0xff);
pub const TRASH_HOT: Color = Color::rgba(0xf6, 0xc9, 0xc4, 0xff);

/// The banner's colour for a day before, on or after today.
pub const BANNER_PAST: Color = Color::rgba(0xdd, 0xdd, 0xd8, 0xff);
pub const BANNER_TODAY: Color = Color::rgba(0xf2, 0xd8, 0x8a, 0xff);
pub const BANNER_FUTURE: Color = Color::rgba(0xc8, 0xdc, 0xee, 0xff);

pub const PLANNED_FILL: Color = Color::rgba(0xcf, 0xdf, 0xf5, 0xff);
pub const PLANNED_EDGE: Color = Color::rgba(0x4f, 0x78, 0xb8, 0xff);
pub const ACTUAL_FILL: Color = Color::rgba(0xd3, 0xec, 0xcf, 0xff);
pub const ACTUAL_EDGE: Color = Color::rgba(0x4d, 0x8f, 0x48, 0xff);

pub const NODE_ROOT: Color = Color::rgba(0xf2, 0xd8, 0x8a, 0xff);
pub const NODE_MILESTONE: Color = Color::rgba(0xcf, 0xdf, 0xf5, 0xff);
pub const NODE_TASK: Color = Color::rgba(0xff, 0xff, 0xff, 0xff);
pub const NODE_EDGE: Color = Color::rgba(0x77, 0x67, 0x67, 0xff);

pub const FONT: u16 = 14;
pub const SMALL_FONT: u16 = 12;
pub const BANNER_FONT: u16 = 22;

/// The priority marks, 1 (red, most urgent) to 5 (blue), as the Win32
/// Task's bitmaps colour them.
pub fn priority_color(priority: i32) -> Option<Color> {
    Some(match priority {
        1 => Color::rgba(0xe0, 0x3a, 0x3a, 0xff),
        2 => Color::rgba(0xec, 0x8a, 0x2c, 0xff),
        3 => Color::rgba(0xe2, 0xc2, 0x2a, 0xff),
        4 => Color::rgba(0x52, 0xa8, 0x4c, 0xff),
        5 => Color::rgba(0x3c, 0x86, 0xd0, 0xff),
        _ => return None,
    })
}

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

/// Fills the part of `rect` inside `clip`.
pub fn fill_clipped(scene: &mut Vec<PaintOp>, rect: Rect, clip: Rect, color: Color) {
    if let Some(rect) = intersect(rect, clip) {
        fill(scene, rect, color);
    }
}

pub fn line(scene: &mut Vec<PaintOp>, from: Point, to: Point, color: Color) {
    scene.push(PaintOp::Line { from, to, color });
}

/// Draws the part of the line from `from` to `to` inside `clip`.
pub fn line_clipped(scene: &mut Vec<PaintOp>, from: Point, to: Point, clip: Rect, color: Color) {
    if let Some((from, to)) = clip_line(from, to, clip) {
        line(scene, from, to, color);
    }
}

pub fn text(
    scene: &mut Vec<PaintOp>,
    rect: Rect,
    clip: Option<Rect>,
    text: impl Into<String>,
    font_size: u16,
    color: Color,
    align: HorizontalAlign,
) {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    let clip_rect = match clip {
        Some(clip) => match intersect(rect, clip) {
            Some(visible) => Some(visible),
            None => return,
        },
        None => None,
    };
    scene.push(PaintOp::Text {
        rect,
        clip_rect,
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

/// A plain push button.
pub fn button(scene: &mut Vec<PaintOp>, rect: Rect, label: &str, enabled: bool, hot: bool) {
    fill(scene, rect, if hot && enabled { HOVER } else { BUTTON });
    stroke(scene, rect, BORDER);
    let color = if enabled { TEXT } else { BUTTON_DISABLED_TEXT };
    text(
        scene,
        rect,
        None,
        label,
        FONT,
        color,
        HorizontalAlign::Center,
    );
}

/// A priority mark centred in `rect`, or nothing for none.
pub fn priority_mark(scene: &mut Vec<PaintOp>, rect: Rect, priority: i32) {
    if let Some(color) = priority_color(priority) {
        let radius = (rect.width.min(rect.height) / 2.0 - 1.0).max(2.0);
        scene.push(PaintOp::FillCircle {
            center: center(rect),
            radius,
            color,
        });
    }
}

/// A waste bin outline in `rect`, for the trash drop target.
pub fn trash_icon(scene: &mut Vec<PaintOp>, rect: Rect, color: Color) {
    let w = rect.width.min(rect.height) * 0.5;
    let c = center(rect);
    let top = c.y - w * 0.55;
    let lid = Rect {
        x: c.x - w * 0.6,
        y: top,
        width: w * 1.2,
        height: 2.0,
    };
    fill(scene, lid, color);
    fill(
        scene,
        Rect {
            x: c.x - w * 0.15,
            y: top - 3.0,
            width: w * 0.3,
            height: 3.0,
        },
        color,
    );
    let body = Rect {
        x: c.x - w * 0.45,
        y: top + 4.0,
        width: w * 0.9,
        height: w * 1.05,
    };
    stroke(scene, body, color);
    for i in 1..3 {
        let x = body.x + body.width * i as f32 / 3.0;
        line(
            scene,
            Point { x, y: body.y + 3.0 },
            Point {
                x,
                y: body.bottom() - 3.0,
            },
            color,
        );
    }
}

pub fn center(rect: Rect) -> Point {
    Point {
        x: rect.x + rect.width / 2.0,
        y: rect.y + rect.height / 2.0,
    }
}

pub fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
    Rect {
        x,
        y,
        width: width.max(0.0),
        height: height.max(0.0),
    }
}

pub fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    (right > x && bottom > y).then_some(Rect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    })
}

/// Liang-Barsky clipping of a segment to a rectangle.
pub fn clip_line(from: Point, to: Point, clip: Rect) -> Option<(Point, Point)> {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let mut t0: f32 = 0.0;
    let mut t1: f32 = 1.0;
    let checks = [
        (-dx, from.x - clip.x),
        (dx, clip.right() - from.x),
        (-dy, from.y - clip.y),
        (dy, clip.bottom() - from.y),
    ];
    for (p, q) in checks {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
        }
    }
    (t0 <= t1).then_some({
        (
            Point {
                x: from.x + t0 * dx,
                y: from.y + t0 * dy,
            },
            Point {
                x: from.x + t1 * dx,
                y: from.y + t1 * dy,
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_clipped_to_the_rectangle() {
        let clip = rect(0.0, 0.0, 10.0, 10.0);
        let (a, b) = clip_line(Point { x: -5.0, y: 5.0 }, Point { x: 15.0, y: 5.0 }, clip).unwrap();
        assert_eq!((a.x, b.x), (0.0, 10.0));
        assert!(clip_line(Point { x: -5.0, y: -1.0 }, Point { x: 15.0, y: -1.0 }, clip).is_none());
        let (a, b) = clip_line(Point { x: 2.0, y: 2.0 }, Point { x: 4.0, y: 4.0 }, clip).unwrap();
        assert_eq!((a.x, b.y), (2.0, 4.0));
    }

    #[test]
    fn rectangles_intersect() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(5.0, 5.0, 10.0, 10.0);
        assert_eq!(intersect(a, b), Some(rect(5.0, 5.0, 5.0, 5.0)));
        assert_eq!(intersect(a, rect(20.0, 0.0, 1.0, 1.0)), None);
    }
}
