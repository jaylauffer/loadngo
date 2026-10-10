//! Small painting helpers shared by the editor's panels.

use ui_core::{
    Color, HorizontalAlign, PaintOp, Point, Rect, TextLayoutMode, TextOverflow, TextStyle,
    VerticalAlign,
};

pub fn fill(scene: &mut Vec<PaintOp>, rect: Rect, color: Color) {
    if rect.width > 0.0 && rect.height > 0.0 {
        scene.push(PaintOp::FillRect { rect, color });
    }
}

/// The overlap of `a` and `b` (zero-sized when they do not overlap).
pub fn intersect(a: Rect, b: Rect) -> Rect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    Rect {
        x,
        y,
        width: (right - x).max(0.0),
        height: (bottom - y).max(0.0),
    }
}

pub fn hline(scene: &mut Vec<PaintOp>, x: f32, y: f32, width: f32, color: Color) {
    scene.push(PaintOp::Line {
        from: Point { x, y },
        to: Point { x: x + width, y },
        color,
    });
}

pub fn vline(scene: &mut Vec<PaintOp>, x: f32, y: f32, height: f32, color: Color) {
    scene.push(PaintOp::Line {
        from: Point { x, y },
        to: Point { x, y: y + height },
        color,
    });
}

/// One line of text, vertically centered in `rect`, ending in an ellipsis
/// when it does not fit.
pub fn label(
    scene: &mut Vec<PaintOp>,
    text: &str,
    rect: Rect,
    color: Color,
    font_size: u16,
    align: HorizontalAlign,
) {
    label_in(scene, text, rect, rect, color, font_size, align);
}

/// [`label`], drawn only where it overlaps `clip`.
pub fn label_in(
    scene: &mut Vec<PaintOp>,
    text: &str,
    rect: Rect,
    clip: Rect,
    color: Color,
    font_size: u16,
    align: HorizontalAlign,
) {
    let clip = intersect(rect, clip);
    if text.is_empty() || clip.width <= 0.0 || clip.height <= 0.0 {
        return;
    }
    scene.push(PaintOp::Text {
        rect,
        clip_rect: Some(clip),
        text: text.to_string(),
        style: TextStyle {
            color,
            font_size,
            horizontal_align: align,
            vertical_align: VerticalAlign::Middle,
            layout_mode: TextLayoutMode::SingleLine,
            overflow: TextOverflow::EllipsisEnd,
            ..TextStyle::default()
        },
    });
}

/// A small filled triangle pointing right (`open` false) or down (`open`
/// true), centered on `center`, drawn from horizontal or vertical lines so
/// it needs no glyph.
pub fn disclosure(scene: &mut Vec<PaintOp>, center: Point, open: bool, color: Color) {
    let half = 4.0_f32;
    let steps = 5;
    for step in 0..steps {
        let t = step as f32 / (steps - 1) as f32;
        if open {
            // Rows from the wide top edge to the point.
            let y = center.y - half * 0.6 + t * half * 1.2;
            let width = half * 2.0 * (1.0 - t);
            hline(scene, center.x - width / 2.0, y, width, color);
        } else {
            let x = center.x - half * 0.6 + t * half * 1.2;
            let height = half * 2.0 * (1.0 - t);
            vline(scene, x, center.y - height / 2.0, height, color);
        }
    }
}

/// A flat button: background, optional hover tint, centered label.
pub fn button(scene: &mut Vec<PaintOp>, rect: Rect, text: &str, hover: bool, enabled: bool) {
    let background = if hover && enabled {
        crate::theme::BUTTON_HOVER
    } else {
        crate::theme::BUTTON
    };
    fill(scene, rect, background);
    let color = if enabled {
        crate::theme::TEXT
    } else {
        crate::theme::TEXT_DIM
    };
    label(
        scene,
        text,
        rect,
        color,
        crate::theme::UI_FONT,
        HorizontalAlign::Center,
    );
}
