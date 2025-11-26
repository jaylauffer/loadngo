use std::time::Duration;

use loadngo_host_core::{
    FrameDemand, HostKey, RenderOp, RenderTextHorizontalAlign, RenderTextLayoutMode,
    RenderTextOverflow, RenderTextStyle, RenderTextVerticalAlign, WindowDescriptor,
};
use ui_core::{
    multiline_line_step, single_line_text_box_height, ButtonModel, Color, HorizontalAlign, Insets,
    LabelModel, ListRowModel, PanelModel, Rect, TextBlockModel, TextLayoutMode, TextOverflow,
    VerticalAlign,
};

const WINDOW_WIDTH: i32 = 1440;
const WINDOW_HEIGHT: i32 = 960;
const OUTER_GUTTER: f32 = 24.0;
const PANEL_GAP: f32 = 20.0;
const SECTION_TITLE_FONT: u16 = 22;
const BODY_FONT: u16 = 18;
const CAPTION_FONT: u16 = 15;
const SECTION_HEADER_HEIGHT: f32 = 36.0;
const SECTION_INSET: Insets = Insets {
    left: 18.0,
    top: 16.0,
    right: 18.0,
    bottom: 18.0,
};

fn main() {
    loadngo_host_desktop::launch(window_descriptor(), None, async move {
        run_text_harness().await;
    });
}

async fn run_text_harness() {
    loop {
        let frame = loadngo_host_desktop::capture_frame();
        if frame.input.key_pressed(HostKey::Escape) {
            break;
        }

        let layout = HarnessLayout::new(frame.surface.width, frame.surface.height);
        loadngo_host_desktop::clear(Color::rgba(0x12, 0x16, 0x1f, 0xff));
        loadngo_host_desktop::render_ops(&build_direct_text_ops(&layout), None);
        let widget_ops = build_widget_ops(&layout);
        loadngo_host_desktop::render_widget_paint_ops(&widget_ops);
        loadngo_host_desktop::next_frame(FrameDemand::after(Duration::from_millis(16))).await;
    }
}

fn window_descriptor() -> WindowDescriptor {
    WindowDescriptor {
        title: "loadngo text harness".to_string(),
        width: Some(WINDOW_WIDTH),
        height: Some(WINDOW_HEIGHT),
        high_dpi: true,
        linux_wm_class: Some("loadngo-text-harness"),
    }
}

#[derive(Debug, Clone, Copy)]
struct HarnessLayout {
    direct_text: Rect,
    buttons: Rect,
    list_rows: Rect,
    multiline: Rect,
}

impl HarnessLayout {
    fn new(surface_width: f32, surface_height: f32) -> Self {
        let outer = Rect {
            x: OUTER_GUTTER,
            y: OUTER_GUTTER,
            width: (surface_width - OUTER_GUTTER * 2.0).max(320.0),
            height: (surface_height - OUTER_GUTTER * 2.0).max(320.0),
        };
        let column_gap = PANEL_GAP;
        let row_gap = PANEL_GAP;
        let column_width = ((outer.width - column_gap) * 0.5).max(120.0);
        let row_height = ((outer.height - row_gap) * 0.5).max(120.0);
        let left_x = outer.x;
        let right_x = left_x + column_width + column_gap;
        let top_y = outer.y;
        let bottom_y = top_y + row_height + row_gap;

        Self {
            direct_text: Rect {
                x: left_x,
                y: top_y,
                width: column_width,
                height: row_height,
            },
            buttons: Rect {
                x: right_x,
                y: top_y,
                width: column_width,
                height: row_height,
            },
            list_rows: Rect {
                x: left_x,
                y: bottom_y,
                width: column_width,
                height: row_height,
            },
            multiline: Rect {
                x: right_x,
                y: bottom_y,
                width: column_width,
                height: row_height,
            },
        }
    }
}

fn build_widget_ops(layout: &HarnessLayout) -> Vec<ui_core::PaintOp> {
    let mut ops = Vec::new();
    paint_panel_shell(&mut ops, layout.direct_text, "Direct RenderOp::Text");
    paint_panel_shell(&mut ops, layout.buttons, "ButtonModel");
    paint_panel_shell(&mut ops, layout.list_rows, "ListRowModel + LabelModel");
    paint_panel_shell(&mut ops, layout.multiline, "TextBlockModel");
    paint_buttons_panel(&mut ops, layout.buttons);
    paint_list_rows_panel(&mut ops, layout.list_rows);
    paint_multiline_panel(&mut ops, layout.multiline);
    ops
}

fn build_direct_text_ops(layout: &HarnessLayout) -> Vec<RenderOp> {
    let mut ops = Vec::new();
    let content = section_content_rect(layout.direct_text);
    let sample_gap = 14.0;
    let sample_height = single_line_text_box_height(BODY_FONT) + 18.0;
    let sample_width = content.width;
    let mut y = content.y;

    for (label, vertical_align) in [
        ("Top aligned single-line", RenderTextVerticalAlign::Top),
        (
            "Middle aligned single-line",
            RenderTextVerticalAlign::Middle,
        ),
        (
            "Bottom aligned single-line",
            RenderTextVerticalAlign::Bottom,
        ),
    ] {
        let rect = Rect {
            x: content.x,
            y,
            width: sample_width,
            height: sample_height,
        };
        ops.push(RenderOp::StrokeRect {
            rect,
            color: Color::rgba(0x86, 0x8d, 0xa0, 0xff),
            thickness: 1,
        });
        ops.push(RenderOp::Text {
            rect,
            text: label.to_string(),
            style: RenderTextStyle {
                color: Color::rgba(0xf2, 0xf5, 0xfb, 0xff),
                font_size: BODY_FONT,
                horizontal_align: RenderTextHorizontalAlign::Center,
                vertical_align,
                vertical_metric_mode:
                    loadngo_host_core::RenderTextVerticalMetricMode::LogicalLineBox,
                layout_mode: RenderTextLayoutMode::SingleLine,
                overflow: RenderTextOverflow::Clip,
            },
        });
        y += sample_height + sample_gap;
    }

    let caption_y = y + 6.0;
    let caption_rect = Rect {
        x: content.x,
        y: caption_y,
        width: sample_width,
        height: single_line_text_box_height(CAPTION_FONT),
    };
    ops.push(RenderOp::Text {
        rect: caption_rect,
        text: "Runtime-style narration lines".to_string(),
        style: RenderTextStyle {
            color: Color::rgba(0xc7, 0xd1, 0xe0, 0xff),
            font_size: CAPTION_FONT,
            horizontal_align: RenderTextHorizontalAlign::Left,
            vertical_align: RenderTextVerticalAlign::Top,
            vertical_metric_mode: loadngo_host_core::RenderTextVerticalMetricMode::LogicalLineBox,
            layout_mode: RenderTextLayoutMode::SingleLine,
            overflow: RenderTextOverflow::Clip,
        },
    });

    let speaker_y = caption_y + single_line_text_box_height(CAPTION_FONT) + 10.0;
    let speaker_rect = Rect {
        x: content.x,
        y: speaker_y,
        width: sample_width,
        height: single_line_text_box_height(BODY_FONT),
    };
    ops.push(RenderOp::Text {
        rect: speaker_rect,
        text: "Narrator".to_string(),
        style: RenderTextStyle {
            color: Color::rgba(0xff, 0xe1, 0xb8, 0xff),
            font_size: BODY_FONT,
            horizontal_align: RenderTextHorizontalAlign::Left,
            vertical_align: RenderTextVerticalAlign::Top,
            vertical_metric_mode: loadngo_host_core::RenderTextVerticalMetricMode::LogicalLineBox,
            layout_mode: RenderTextLayoutMode::SingleLine,
            overflow: RenderTextOverflow::Clip,
        },
    });

    let line_step = multiline_line_step(BODY_FONT);
    let mut line_y = speaker_y + single_line_text_box_height(BODY_FONT) + 6.0;
    for line in [
        "This harness isolates the shared desktop text path.",
        "Direct text, widget buttons, and list rows should agree.",
        "Escape closes the window.",
    ] {
        let rect = Rect {
            x: content.x,
            y: line_y,
            width: sample_width,
            height: single_line_text_box_height(BODY_FONT),
        };
        ops.push(RenderOp::Text {
            rect,
            text: line.to_string(),
            style: RenderTextStyle {
                color: Color::rgba(0xf2, 0xf5, 0xfb, 0xff),
                font_size: BODY_FONT,
                horizontal_align: RenderTextHorizontalAlign::Left,
                vertical_align: RenderTextVerticalAlign::Top,
                vertical_metric_mode:
                    loadngo_host_core::RenderTextVerticalMetricMode::LogicalLineBox,
                layout_mode: RenderTextLayoutMode::SingleLine,
                overflow: RenderTextOverflow::Clip,
            },
        });
        line_y += line_step;
    }

    ops
}

fn paint_buttons_panel(scene: &mut Vec<ui_core::PaintOp>, section: Rect) {
    let content = section_content_rect(section);
    let note_rect = Rect {
        x: content.x,
        y: content.y,
        width: content.width,
        height: single_line_text_box_height(CAPTION_FONT),
    };
    let mut note = LabelModel::new(
        "Buttons should center text vertically without caller offsets.",
        note_rect,
    );
    note.style.font_size = CAPTION_FONT;
    note.style.color = Color::rgba(0xc7, 0xd1, 0xe0, 0xff);
    note.paint(scene);

    let button_height = 52.0;
    let mut y = content.y + single_line_text_box_height(CAPTION_FONT) + 14.0;
    for text in ["Menu", "Return To Story", "Choice Overflow Example..."] {
        let button_rect = Rect {
            x: content.x,
            y,
            width: content.width,
            height: button_height,
        };
        let button = ButtonModel::new(text, button_rect);
        button.paint(scene);
        y += button_height + 14.0;
    }
}

fn paint_list_rows_panel(scene: &mut Vec<ui_core::PaintOp>, section: Rect) {
    let content = section_content_rect(section);
    let note_rect = Rect {
        x: content.x,
        y: content.y,
        width: content.width,
        height: single_line_text_box_height(CAPTION_FONT),
    };
    let mut note = LabelModel::new("Editor-style fixed-height rows", note_rect);
    note.style.font_size = CAPTION_FONT;
    note.style.color = Color::rgba(0xc7, 0xd1, 0xe0, 0xff);
    note.paint(scene);

    let row_height = (single_line_text_box_height(BODY_FONT) + 8.0).max(34.0);
    let mut y = content.y + single_line_text_box_height(CAPTION_FONT) + 14.0;
    for (index, text) in [
        "start",
        "kitchen_morning",
        "choice_branch_with_longer_name_for_elision",
        "credits",
    ]
    .into_iter()
    .enumerate()
    {
        let row_rect = Rect {
            x: content.x,
            y,
            width: content.width,
            height: row_height,
        };
        let mut row = ListRowModel::new(row_rect);
        row.background = Some(if index == 1 {
            Color::rgba(0x34, 0x5b, 0x99, 0xd8)
        } else {
            Color::rgba(0x1d, 0x23, 0x30, 0xcc)
        });
        row.border = Some(Color::rgba(0x86, 0x8d, 0xa0, 0xff));
        row.paint(scene);

        let body_rect = row.single_line_body_rect(BODY_FONT, 0.0, 0.0, 0.0);
        let mut label = LabelModel::new(text, body_rect);
        label.style.font_size = BODY_FONT;
        label.style.horizontal_align = HorizontalAlign::Left;
        label.style.vertical_align = VerticalAlign::Middle;
        label.style.layout_mode = TextLayoutMode::SingleLine;
        label.style.overflow = TextOverflow::EllipsisEnd;
        label.style.color = if index == 1 {
            Color::rgba(0xf6, 0xf8, 0xfc, 0xff)
        } else {
            Color::rgba(0xe0, 0xe6, 0xf2, 0xff)
        };
        label.paint(scene);

        y += row_height + 10.0;
    }
}

fn paint_multiline_panel(scene: &mut Vec<ui_core::PaintOp>, section: Rect) {
    let content = section_content_rect(section);
    let caption_rect = Rect {
        x: content.x,
        y: content.y,
        width: content.width,
        height: single_line_text_box_height(CAPTION_FONT),
    };
    let mut note = LabelModel::new("Multiline text block", caption_rect);
    note.style.font_size = CAPTION_FONT;
    note.style.color = Color::rgba(0xc7, 0xd1, 0xe0, 0xff);
    note.paint(scene);

    let block_rect = Rect {
        x: content.x,
        y: content.y + single_line_text_box_height(CAPTION_FONT) + 14.0,
        width: content.width,
        height: content.height - single_line_text_box_height(CAPTION_FONT) - 14.0,
    };
    let mut block_panel = PanelModel::new(block_rect);
    block_panel.background = Some(Color::rgba(0x1d, 0x23, 0x30, 0xcc));
    block_panel.border = Some(Color::rgba(0x86, 0x8d, 0xa0, 0xff));
    block_panel.padding = Insets {
        left: 12.0,
        top: 12.0,
        right: 12.0,
        bottom: 12.0,
    };
    block_panel.paint(scene);

    let mut block = TextBlockModel::new(
        "This sample exercises the shared multiline path.\n\
The current contract uses ui_core::multiline_line_step(font_size).\n\
Per-style line spacing is still a follow-up, but placement should remain stable.",
        block_panel.content_rect(),
    );
    block.style.font_size = BODY_FONT;
    block.style.color = Color::rgba(0xf2, 0xf5, 0xfb, 0xff);
    block.paint(scene);
}

fn paint_panel_shell(scene: &mut Vec<ui_core::PaintOp>, bounds: Rect, title: &str) {
    let mut panel = PanelModel::new(bounds);
    panel.background = Some(Color::rgba(0x1a, 0x20, 0x2c, 0xf4));
    panel.border = Some(Color::rgba(0x6f, 0x7e, 0x98, 0xff));
    panel.paint(scene);

    let title_rect = Rect {
        x: bounds.x + SECTION_INSET.left,
        y: bounds.y + SECTION_INSET.top,
        width: bounds.width - SECTION_INSET.left - SECTION_INSET.right,
        height: SECTION_HEADER_HEIGHT,
    };
    let mut title_label = LabelModel::new(title, title_rect);
    title_label.style.font_size = SECTION_TITLE_FONT;
    title_label.style.color = Color::rgba(0xf6, 0xf8, 0xfc, 0xff);
    title_label.style.vertical_align = VerticalAlign::Middle;
    title_label.paint(scene);
}

fn section_content_rect(bounds: Rect) -> Rect {
    Rect {
        x: bounds.x + SECTION_INSET.left,
        y: bounds.y + SECTION_INSET.top + SECTION_HEADER_HEIGHT + 10.0,
        width: (bounds.width - SECTION_INSET.left - SECTION_INSET.right).max(0.0),
        height: (bounds.height
            - SECTION_INSET.top
            - SECTION_INSET.bottom
            - SECTION_HEADER_HEIGHT
            - 10.0)
            .max(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::{section_content_rect, HarnessLayout};
    use ui_core::{multiline_line_step, single_line_text_box_height, Rect};

    #[test]
    fn harness_layout_produces_non_overlapping_quadrants() {
        let layout = HarnessLayout::new(1440.0, 960.0);

        assert!(layout.direct_text.right() <= layout.buttons.x);
        assert!(layout.direct_text.bottom() <= layout.list_rows.y);
        assert!(layout.list_rows.right() <= layout.multiline.x);
    }

    #[test]
    fn section_content_stays_inside_panel_bounds() {
        let panel = Rect {
            x: 24.0,
            y: 24.0,
            width: 400.0,
            height: 300.0,
        };
        let content = section_content_rect(panel);

        assert!(content.x >= panel.x);
        assert!(content.y >= panel.y);
        assert!(content.right() <= panel.right());
        assert!(content.bottom() <= panel.bottom());
    }

    #[test]
    fn shared_text_box_metrics_keep_multiline_step_below_line_box() {
        assert!(single_line_text_box_height(18) > multiline_line_step(18));
    }
}
