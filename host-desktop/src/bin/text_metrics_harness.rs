use std::time::Duration;

use loadngo_host_core::{
    FrameDemand, HostKey, RenderOp, RenderTextHorizontalAlign, RenderTextLayoutMode,
    RenderTextOverflow, RenderTextStyle, RenderTextVerticalAlign, RenderTextVerticalMetricMode,
    WindowDescriptor,
};
use ui_core::{single_line_text_box_height, Color, Rect};

const WINDOW_WIDTH: i32 = 1520;
const WINDOW_HEIGHT: i32 = 980;
const OUTER_GUTTER: f32 = 24.0;
const PANEL_GAP: f32 = 20.0;
const PANEL_PADDING: f32 = 18.0;
const HEADER_HEIGHT: f32 = 34.0;
const TITLE_FONT: u16 = 22;
const BODY_FONT: u16 = 18;
const CAPTION_FONT: u16 = 14;
const ROW_HEIGHT: f32 = 44.0;
const ROW_GAP: f32 = 10.0;
const TAB_HEIGHT: f32 = 42.0;
const SAMPLES: &[&str] = &["123", "...", "ooo", "Ops(", "gggg", "T", "MMMMM", "WWWWW"];
const TAB_SAMPLES: &[&str] = &["123", "...", "ooo"];

fn main() {
    loadngo_host_desktop::launch(window_descriptor(), None, async move {
        run_text_metrics_harness().await;
    });
}

fn window_descriptor() -> WindowDescriptor {
    WindowDescriptor {
        title: "loadngo text metrics harness".to_string(),
        width: Some(WINDOW_WIDTH),
        height: Some(WINDOW_HEIGHT),
        high_dpi: true,
        linux_wm_class: Some("loadngo-text-metrics-harness"),
    }
}

async fn run_text_metrics_harness() {
    let metrics = loadngo_host_desktop::measure_font_line_metrics(None, BODY_FONT, 1.0);
    eprintln!("text_metrics_harness BODY_FONT={BODY_FONT} {}", metrics_summary(metrics));
    log_sample_deltas();
    loop {
        let frame = loadngo_host_desktop::capture_frame();
        if frame.input.key_pressed(HostKey::Escape) {
            break;
        }

        let layout = HarnessLayout::new(frame.surface.width, frame.surface.height);
        let ops = build_ops(&layout);
        loadngo_host_desktop::clear(Color::rgba(0x11, 0x15, 0x1d, 0xff));
        loadngo_host_desktop::render_ops(&ops, None);
        loadngo_host_desktop::next_frame(FrameDemand::after(Duration::from_millis(16))).await;
    }
}

fn log_sample_deltas() {
    for (label, mode) in [
        ("LogicalLineBox", RenderTextVerticalMetricMode::LogicalLineBox),
        ("VisibleInk", RenderTextVerticalMetricMode::VisibleInk),
    ] {
        eprintln!("{label} sample deltas:");
        for sample in SAMPLES {
            let rect = Rect {
                x: 0.0,
                y: 0.0,
                width: 440.0,
                height: ROW_HEIGHT,
            };
            let placement = loadngo_gfx_metal::debug_text_placement(
                &loadngo_renderer::TextRequest {
                    rect,
                    clip_rect: None,
                    text: (*sample).to_string(),
                    style: RenderTextStyle {
                        color: Color::rgba(0xff, 0xff, 0xff, 0xff),
                        font_size: BODY_FONT,
                        horizontal_align: RenderTextHorizontalAlign::Center,
                        vertical_align: RenderTextVerticalAlign::Middle,
                        vertical_metric_mode: mode.clone(),
                        layout_mode: RenderTextLayoutMode::SingleLine,
                        overflow: RenderTextOverflow::Clip,
                    },
                    direction: loadngo_renderer::TextDirection::Auto,
                    script: loadngo_renderer::TextScript::Auto,
                    language: None,
                },
                None,
            )
            .expect("debug text placement should succeed");
            let rect_center = rect.y + rect.height * 0.5;
            let logical_center =
                placement.y + placement.logical_top_in_display + placement.logical_height * 0.5;
            let opaque_center =
                placement.y + placement.opaque_top_in_display + placement.opaque_height * 0.5;
            eprintln!(
                "  {:<7} logical_delta={:+.2} opaque_delta={:+.2} y={:.2} logical_top={:.2} logical_h={:.2} opaque_top={:.2} opaque_h={:.2}",
                sample,
                logical_center - rect_center,
                opaque_center - rect_center,
                placement.y,
                placement.logical_top_in_display,
                placement.logical_height,
                placement.opaque_top_in_display,
                placement.opaque_height
            );
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct HarnessLayout {
    logical_panel: Rect,
    visible_panel: Rect,
}

impl HarnessLayout {
    fn new(surface_width: f32, surface_height: f32) -> Self {
        let outer = Rect {
            x: OUTER_GUTTER,
            y: OUTER_GUTTER,
            width: (surface_width - OUTER_GUTTER * 2.0).max(640.0),
            height: (surface_height - OUTER_GUTTER * 2.0).max(480.0),
        };
        let panel_width = ((outer.width - PANEL_GAP) * 0.5).max(260.0);
        Self {
            logical_panel: Rect {
                x: outer.x,
                y: outer.y,
                width: panel_width,
                height: outer.height,
            },
            visible_panel: Rect {
                x: outer.x + panel_width + PANEL_GAP,
                y: outer.y,
                width: panel_width,
                height: outer.height,
            },
        }
    }
}

fn build_ops(layout: &HarnessLayout) -> Vec<RenderOp> {
    let mut ops = Vec::new();
    let metrics = loadngo_host_desktop::measure_font_line_metrics(None, BODY_FONT, 1.0);
    paint_panel(
        &mut ops,
        layout.logical_panel,
        "LogicalLineBox",
        "Font-based shared line box. Identical strings should share stable top/middle/baseline behavior.",
        RenderTextVerticalMetricMode::LogicalLineBox,
        &metrics_summary(metrics),
    );
    paint_panel(
        &mut ops,
        layout.visible_panel,
        "VisibleInk",
        "Ink-based alignment. Useful for raw rendering, but sibling controls will drift by content.",
        RenderTextVerticalMetricMode::VisibleInk,
        &metrics_summary(metrics),
    );
    ops
}

fn paint_panel(
    ops: &mut Vec<RenderOp>,
    bounds: Rect,
    title: &str,
    caption: &str,
    metric_mode: RenderTextVerticalMetricMode,
    metrics_summary: &str,
) {
    ops.push(RenderOp::FillRect {
        rect: bounds,
        color: Color::rgba(0x18, 0x20, 0x2c, 0xf4),
    });
    ops.push(RenderOp::StrokeRect {
        rect: bounds,
        color: Color::rgba(0x73, 0x82, 0x9c, 0xff),
        thickness: 1,
    });

    let content = Rect {
        x: bounds.x + PANEL_PADDING,
        y: bounds.y + PANEL_PADDING,
        width: (bounds.width - PANEL_PADDING * 2.0).max(0.0),
        height: (bounds.height - PANEL_PADDING * 2.0).max(0.0),
    };
    let title_rect = Rect {
        x: content.x,
        y: content.y,
        width: content.width,
        height: HEADER_HEIGHT,
    };
    ops.push(RenderOp::Text {
        rect: title_rect,
        text: title.to_string(),
        style: RenderTextStyle {
            color: Color::rgba(0xf4, 0xf7, 0xfb, 0xff),
            font_size: TITLE_FONT,
            horizontal_align: RenderTextHorizontalAlign::Left,
            vertical_align: RenderTextVerticalAlign::Middle,
            vertical_metric_mode: RenderTextVerticalMetricMode::LogicalLineBox,
            layout_mode: RenderTextLayoutMode::SingleLine,
            overflow: RenderTextOverflow::Clip,
        },
    });

    let caption_rect = Rect {
        x: content.x,
        y: title_rect.bottom() + 4.0,
        width: content.width,
        height: single_line_text_box_height(CAPTION_FONT),
    };
    ops.push(RenderOp::Text {
        rect: caption_rect,
        text: caption.to_string(),
        style: RenderTextStyle {
            color: Color::rgba(0xc8, 0xd2, 0xe3, 0xff),
            font_size: CAPTION_FONT,
            horizontal_align: RenderTextHorizontalAlign::Left,
            vertical_align: RenderTextVerticalAlign::Top,
            vertical_metric_mode: RenderTextVerticalMetricMode::LogicalLineBox,
            layout_mode: RenderTextLayoutMode::SingleLine,
            overflow: RenderTextOverflow::Clip,
        },
    });

    let metrics_rect = Rect {
        x: content.x,
        y: caption_rect.bottom() + 2.0,
        width: content.width,
        height: single_line_text_box_height(CAPTION_FONT),
    };
    ops.push(RenderOp::Text {
        rect: metrics_rect,
        text: metrics_summary.to_string(),
        style: RenderTextStyle {
            color: Color::rgba(0x9f, 0xaa, 0xbe, 0xff),
            font_size: CAPTION_FONT,
            horizontal_align: RenderTextHorizontalAlign::Left,
            vertical_align: RenderTextVerticalAlign::Top,
            vertical_metric_mode: RenderTextVerticalMetricMode::LogicalLineBox,
            layout_mode: RenderTextLayoutMode::SingleLine,
            overflow: RenderTextOverflow::Clip,
        },
    });

    let sample_top = metrics_rect.bottom() + 16.0;
    let sample_width = content.width;
    let mut y = sample_top;
    for sample in SAMPLES {
        let rect = Rect {
            x: content.x,
            y,
            width: sample_width,
            height: ROW_HEIGHT,
        };
        paint_sample_row(ops, rect, sample, metric_mode.clone());
        y += ROW_HEIGHT + ROW_GAP;
    }

    let tab_caption_rect = Rect {
        x: content.x,
        y: y + 8.0,
        width: content.width,
        height: single_line_text_box_height(CAPTION_FONT),
    };
    ops.push(RenderOp::Text {
        rect: tab_caption_rect,
        text: "Three sibling tabs".to_string(),
        style: RenderTextStyle {
            color: Color::rgba(0xd4, 0xda, 0xe7, 0xff),
            font_size: CAPTION_FONT,
            horizontal_align: RenderTextHorizontalAlign::Left,
            vertical_align: RenderTextVerticalAlign::Top,
            vertical_metric_mode: RenderTextVerticalMetricMode::LogicalLineBox,
            layout_mode: RenderTextLayoutMode::SingleLine,
            overflow: RenderTextOverflow::Clip,
        },
    });

    let tab_y = tab_caption_rect.bottom() + 10.0;
    let tab_gap = 8.0;
    let tab_width = ((content.width - tab_gap * 2.0) / 3.0).max(80.0);
    for (index, sample) in TAB_SAMPLES.iter().enumerate() {
        let rect = Rect {
            x: content.x + index as f32 * (tab_width + tab_gap),
            y: tab_y,
            width: tab_width,
            height: TAB_HEIGHT,
        };
        paint_tab_sample(ops, rect, sample, metric_mode.clone());
    }
}

fn metrics_summary(metrics: loadngo_gfx_metal::FontLineMetrics) -> String {
    format!(
        "asc {:.0} desc {:.0} lead {:.0} ink {:.0}/{:.0}/{:.0} base {:.0} line {:.0} box {:.0} step {:.0} pad {:.0}/{:.0}",
        metrics.ascent,
        metrics.descent,
        metrics.leading,
        metrics.ink_top_from_baseline,
        metrics.ink_bottom_from_baseline,
        metrics.ink_height,
        metrics.baseline_from_top,
        metrics.line_height,
        metrics.line_box_height,
        metrics.line_step,
        metrics.raster_pad_top,
        metrics.raster_pad_bottom
    )
}

fn paint_sample_row(
    ops: &mut Vec<RenderOp>,
    rect: Rect,
    text: &str,
    metric_mode: RenderTextVerticalMetricMode,
) {
    ops.push(RenderOp::StrokeRect {
        rect,
        color: Color::rgba(0x7a, 0x87, 0x9c, 0xff),
        thickness: 1,
    });
    let mid_y = rect.y + rect.height * 0.5;
    ops.push(RenderOp::Line {
        from: ui_core::Point { x: rect.x, y: mid_y },
        to: ui_core::Point {
            x: rect.right(),
            y: mid_y,
        },
        color: Color::rgba(0xff, 0x45, 0x45, 0xff),
        thickness: 2,
    });
    paint_logical_box_overlay(ops, rect, text, metric_mode.clone());
    ops.push(RenderOp::Text {
        rect,
        text: text.to_string(),
        style: RenderTextStyle {
            color: Color::rgba(0xf3, 0xf6, 0xfb, 0xff),
            font_size: BODY_FONT,
            horizontal_align: RenderTextHorizontalAlign::Center,
            vertical_align: RenderTextVerticalAlign::Middle,
            vertical_metric_mode: metric_mode,
            layout_mode: RenderTextLayoutMode::SingleLine,
            overflow: RenderTextOverflow::Clip,
        },
    });
}

fn paint_tab_sample(
    ops: &mut Vec<RenderOp>,
    rect: Rect,
    text: &str,
    metric_mode: RenderTextVerticalMetricMode,
) {
    ops.push(RenderOp::FillRect {
        rect,
        color: Color::rgba(0xe8, 0xe9, 0xef, 0xff),
    });
    ops.push(RenderOp::StrokeRect {
        rect,
        color: Color::rgba(0x8c, 0x94, 0xa5, 0xff),
        thickness: 1,
    });
    let mid_y = rect.y + rect.height * 0.5;
    ops.push(RenderOp::Line {
        from: ui_core::Point { x: rect.x, y: mid_y },
        to: ui_core::Point {
            x: rect.right(),
            y: mid_y,
        },
        color: Color::rgba(0xff, 0x45, 0x45, 0xff),
        thickness: 2,
    });
    paint_logical_box_overlay(ops, rect, text, metric_mode.clone());
    ops.push(RenderOp::Text {
        rect,
        text: text.to_string(),
        style: RenderTextStyle {
            color: Color::rgba(0x22, 0x25, 0x2c, 0xff),
            font_size: BODY_FONT,
            horizontal_align: RenderTextHorizontalAlign::Center,
            vertical_align: RenderTextVerticalAlign::Middle,
            vertical_metric_mode: metric_mode,
            layout_mode: RenderTextLayoutMode::SingleLine,
            overflow: RenderTextOverflow::Clip,
        },
    });
}

fn paint_logical_box_overlay(
    ops: &mut Vec<RenderOp>,
    rect: Rect,
    text: &str,
    metric_mode: RenderTextVerticalMetricMode,
) {
    let style = RenderTextStyle {
        color: Color::rgba(0xff, 0xff, 0xff, 0xff),
        font_size: BODY_FONT,
        horizontal_align: RenderTextHorizontalAlign::Center,
        vertical_align: RenderTextVerticalAlign::Middle,
        vertical_metric_mode: metric_mode,
        layout_mode: RenderTextLayoutMode::SingleLine,
        overflow: RenderTextOverflow::Clip,
    };
    let placement = loadngo_gfx_metal::debug_text_placement(
        &loadngo_renderer::TextRequest {
            rect,
            clip_rect: None,
            text: text.to_string(),
            style,
            direction: loadngo_renderer::TextDirection::Auto,
            script: loadngo_renderer::TextScript::Auto,
            language: None,
        },
        None,
    )
    .expect("debug text placement should succeed");
    let metrics = loadngo_host_desktop::measure_text_metrics(text, None, BODY_FONT, 1.0);
    let logical_rect = Rect {
        x: placement.x,
        y: placement.y + placement.logical_top_in_display,
        width: metrics.width.max(1.0),
        height: placement.logical_height.max(1.0),
    };
    let logical_mid_y = logical_rect.y + logical_rect.height * 0.5;
    let overlay = Color::rgba(0xff, 0xdd, 0x33, 0x99);
    ops.push(RenderOp::StrokeRect {
        rect: logical_rect,
        color: overlay,
        thickness: 1,
    });
    ops.push(RenderOp::Line {
        from: ui_core::Point {
            x: logical_rect.x,
            y: logical_mid_y,
        },
        to: ui_core::Point {
            x: logical_rect.right(),
            y: logical_mid_y,
        },
        color: overlay,
        thickness: 2,
    });
}
