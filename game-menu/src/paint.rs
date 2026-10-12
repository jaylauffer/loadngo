//! Drawing shared by the menu screens: panels, buttons, text.

use loadngo_host_core::{
    RenderOp, RenderTextHorizontalAlign, RenderTextOverflow, RenderTextStyle,
    RenderTextVerticalAlign,
};
use ui_core::{Color, Rect};

use crate::MenuTheme;

/// Gap between a panel and the safe area's edge.
const PANEL_MARGIN: f32 = 16.0;
pub(crate) const PANEL_PADDING: f32 = 24.0;
/// Height of a panel's title row, from the panel's top edge.
pub(crate) const PANEL_HEADER: f32 = 66.0;
const CLOSE_SIZE: f32 = 40.0;

/// A panel of at most `max_width` x `wanted_height`, centred in `safe`.
pub(crate) fn centred_panel(safe: Rect, max_width: f32, wanted_height: f32) -> Rect {
    let width = (safe.width - PANEL_MARGIN * 2.0).min(max_width).max(0.0);
    let height = (safe.height - PANEL_MARGIN * 2.0)
        .min(wanted_height)
        .max(0.0);
    Rect {
        x: safe.x + (safe.width - width) * 0.5,
        y: safe.y + (safe.height - height) * 0.5,
        width,
        height,
    }
}

/// The close ("X") button in a panel's top-right corner. 40 points square,
/// large enough for a finger.
pub(crate) fn close_button_rect(panel: Rect) -> Rect {
    Rect {
        x: panel.x + panel.width - CLOSE_SIZE - 12.0,
        y: panel.y + (PANEL_HEADER - CLOSE_SIZE) * 0.5,
        width: CLOSE_SIZE,
        height: CLOSE_SIZE,
    }
}

pub(crate) fn push_fill(ops: &mut Vec<RenderOp>, rect: Rect, color: Color) {
    ops.push(RenderOp::FillRect { rect, color });
}

/// Scrim over the whole surface, then the panel with its border, title,
/// close button and the divider under the title.
pub(crate) fn push_panel(
    ops: &mut Vec<RenderOp>,
    theme: &MenuTheme,
    surface: Rect,
    panel: Rect,
    title: &str,
    close_highlighted: bool,
) {
    push_fill(ops, surface, theme.scrim);
    push_fill(ops, panel, theme.panel);
    ops.push(RenderOp::StrokeRect {
        rect: panel,
        color: theme.panel_border,
        thickness: 3,
    });
    let close = close_button_rect(panel);
    push_text(
        ops,
        Rect {
            x: panel.x + PANEL_PADDING,
            y: panel.y,
            width: (close.x - panel.x - PANEL_PADDING - 8.0).max(0.0),
            height: PANEL_HEADER,
        },
        title,
        theme.title,
        26,
        RenderTextHorizontalAlign::Left,
    );
    push_button_frame(ops, theme, close, close_highlighted);
    push_text(
        ops,
        close,
        "X",
        theme.text,
        18,
        RenderTextHorizontalAlign::Center,
    );
    push_fill(
        ops,
        Rect {
            x: panel.x + PANEL_PADDING,
            y: panel.y + PANEL_HEADER,
            width: (panel.width - PANEL_PADDING * 2.0).max(0.0),
            height: 1.0,
        },
        theme.panel_border,
    );
}

pub(crate) fn push_button_frame(
    ops: &mut Vec<RenderOp>,
    theme: &MenuTheme,
    rect: Rect,
    highlighted: bool,
) {
    push_fill(
        ops,
        rect,
        if highlighted {
            theme.button_highlight
        } else {
            theme.button
        },
    );
    ops.push(RenderOp::StrokeRect {
        rect,
        color: if highlighted {
            theme.highlight_border
        } else {
            theme.button_border
        },
        thickness: if highlighted { 3 } else { 1 },
    });
}

pub(crate) fn text_style(
    color: Color,
    font_size: u16,
    horizontal_align: RenderTextHorizontalAlign,
) -> RenderTextStyle {
    RenderTextStyle {
        color,
        font_size,
        horizontal_align,
        vertical_align: RenderTextVerticalAlign::Middle,
        overflow: RenderTextOverflow::EllipsisEnd,
        ..RenderTextStyle::default()
    }
}

pub(crate) fn push_text(
    ops: &mut Vec<RenderOp>,
    rect: Rect,
    text: &str,
    color: Color,
    font_size: u16,
    horizontal_align: RenderTextHorizontalAlign,
) {
    ops.push(RenderOp::Text {
        rect,
        text: text.to_string(),
        style: text_style(color, font_size, horizontal_align),
    });
}
