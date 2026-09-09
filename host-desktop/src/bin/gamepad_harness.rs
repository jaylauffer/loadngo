//! Shows every connected gamepad's live state, so a backend can be checked
//! against real hardware rather than against its own unit tests.
//!
//! Gamepad state is the one input modality with no visible trace on screen
//! by default — a wrong axis sign or a mismapped button looks identical to
//! "nothing happened" — so each backend needs somewhere to watch the raw
//! values while pressing things. Runs on any host whose backend fills
//! `InputSnapshot::gamepads`; macOS and Linux do today.

use std::time::Duration;

use loadngo_host_core::{
    FrameDemand, GamepadButton, GamepadSnapshot, HostKey, PointF, WindowDescriptor,
};
use ui_core::{
    Color, HorizontalAlign, Insets, LabelModel, PaintOp, PanelModel, Point, Rect, TextBlockModel,
    VerticalAlign,
};

const WINDOW_WIDTH: i32 = 1180;
const WINDOW_HEIGHT: i32 = 760;
const OUTER_GUTTER: f32 = 24.0;
const PANEL_GAP: f32 = 20.0;
const NOTES_WIDTH: f32 = 320.0;
const TITLE_FONT: u16 = 24;
const BODY_FONT: u16 = 17;
const CAPTION_FONT: u16 = 14;
const PANEL_PADDING: Insets = Insets {
    left: 16.0,
    top: 14.0,
    right: 16.0,
    bottom: 16.0,
};

/// A pad card is fixed-height so several pads stack predictably.
const CARD_HEIGHT: f32 = 232.0;
const CARD_GAP: f32 = 14.0;
const STICK_RADIUS: f32 = 54.0;

/// Matches the deadzone `touch::VirtualJoystick` and the menu navigation
/// code use, so the "shaped" readout shows what a game would actually act
/// on rather than a number only this harness would ever see.
const DISPLAY_DEADZONE: f32 = 0.25;

const INK: Color = Color::rgba(0xf4, 0xf7, 0xfb, 0xff);
const DIM_INK: Color = Color::rgba(0xa8, 0xb6, 0xcc, 0xff);
const PANEL_FILL: Color = Color::rgba(0x1a, 0x22, 0x2f, 0xf0);
const PANEL_EDGE: Color = Color::rgba(0x73, 0x82, 0x9c, 0xff);
const IDLE_CHIP: Color = Color::rgba(0x25, 0x2f, 0x3e, 0xff);
const LIVE_CHIP: Color = Color::rgba(0x3c, 0x8c, 0x5a, 0xff);
const EDGE_CHIP: Color = Color::rgba(0xd8, 0x9b, 0x36, 0xff);

/// Every button the contract defines, in a layout that reads like a pad:
/// face cluster, then d-pad, then shoulders, then the small centre keys.
const BUTTON_ORDER: [(GamepadButton, &str); 15] = [
    (GamepadButton::South, "South"),
    (GamepadButton::East, "East"),
    (GamepadButton::West, "West"),
    (GamepadButton::North, "North"),
    (GamepadButton::DPadUp, "D-Up"),
    (GamepadButton::DPadDown, "D-Down"),
    (GamepadButton::DPadLeft, "D-Left"),
    (GamepadButton::DPadRight, "D-Right"),
    (GamepadButton::LeftShoulder, "L-Shldr"),
    (GamepadButton::RightShoulder, "R-Shldr"),
    (GamepadButton::LeftStick, "L-Stick"),
    (GamepadButton::RightStick, "R-Stick"),
    (GamepadButton::Start, "Start"),
    (GamepadButton::Select, "Select"),
    (GamepadButton::Guide, "Guide"),
];

fn main() {
    loadngo_host_desktop::launch(window_descriptor(), None, async move {
        run_gamepad_harness().await;
    });
}

fn window_descriptor() -> WindowDescriptor {
    WindowDescriptor {
        title: "loadngo gamepad harness".to_string(),
        width: Some(WINDOW_WIDTH),
        height: Some(WINDOW_HEIGHT),
        high_dpi: true,
        linux_wm_class: Some("loadngo-gamepad-harness"),
    }
}

async fn run_gamepad_harness() {
    // Press edges last exactly one frame, which is too short to read. Keep
    // a short history instead, so a button that fired can still be checked
    // after the fact — the difference between "the edge never arrived" and
    // "I blinked" is the whole point of this harness.
    let mut press_log: Vec<String> = Vec::new();
    let mut seen_any_pad = false;

    loop {
        let frame = loadngo_host_desktop::capture_frame();
        if frame.input.key_pressed(HostKey::Escape) {
            break;
        }
        if !frame.input.gamepads.is_empty() {
            seen_any_pad = true;
        }
        for pad in &frame.input.gamepads {
            if !pad.connected {
                push_log(&mut press_log, format!("pad {} disconnected", pad.id));
                continue;
            }
            for (button, name) in BUTTON_ORDER {
                if pad.buttons_pressed.contains(&button) {
                    push_log(&mut press_log, format!("pad {} pressed {name}", pad.id));
                }
            }
        }
        let surface = frame.surface;
        let mut scene = Vec::new();
        paint_notes(&mut scene, surface.height, &press_log);
        paint_pads(
            &mut scene,
            surface.width,
            surface.height,
            &frame.input.gamepads,
            seen_any_pad,
        );

        loadngo_host_desktop::clear(Color::rgba(0x10, 0x15, 0x1d, 0xff));
        loadngo_host_desktop::render_widget_paint_ops(&scene);
        loadngo_host_desktop::next_frame(FrameDemand::after(Duration::from_millis(16))).await;
    }
}

fn push_log(log: &mut Vec<String>, entry: String) {
    log.push(entry);
    let overflow = log.len().saturating_sub(12);
    log.drain(..overflow);
}

fn paint_notes(scene: &mut Vec<PaintOp>, height: f32, press_log: &[String]) {
    let bounds = Rect {
        x: OUTER_GUTTER,
        y: OUTER_GUTTER,
        width: NOTES_WIDTH,
        height: height - OUTER_GUTTER * 2.0,
    };
    let mut panel = PanelModel::new(bounds);
    panel.background = Some(PANEL_FILL);
    panel.border = Some(PANEL_EDGE);
    panel.padding = PANEL_PADDING;
    panel.paint(scene);

    let content = panel.content_rect();
    let title_height = ui_core::single_line_text_box_height(TITLE_FONT);
    let mut title = LabelModel::new(
        "Gamepad Harness",
        Rect {
            height: title_height,
            ..content
        },
    );
    title.style.font_size = TITLE_FONT;
    title.style.vertical_align = VerticalAlign::Middle;
    title.style.color = INK;
    title.paint(scene);

    let mut notes = TextBlockModel::new(
        "Purpose\n\
Watch raw gamepad values while\n\
pressing things, to check a\n\
backend against real hardware.\n\n\
What to confirm\n\
- one card per physical pad,\n\
   not one per device node\n\
- stick UP gives a NEGATIVE y\n\
- triggers sweep 0.00 to 1.00\n\
- each button lights the chip\n\
   matching its position\n\
- unplugging clears the card\n\n\
Chips\n\
green = held this frame\n\
amber = press edge this frame\n\n\
Escape closes the window.",
        Rect {
            x: content.x,
            y: content.y + title_height + 12.0,
            width: content.width,
            height: content.height - title_height - 12.0,
        },
    );
    notes.style.font_size = CAPTION_FONT;
    notes.style.color = DIM_INK;
    notes.paint(scene);

    if press_log.is_empty() {
        return;
    }
    let line_height = ui_core::single_line_text_box_height(CAPTION_FONT);
    let log_height = line_height * (press_log.len() as f32 + 1.0);
    paint_lines(
        scene,
        Rect {
            x: content.x,
            y: content.y + content.height - log_height,
            width: content.width,
            height: log_height,
        },
        std::iter::once("Press edges").chain(press_log.iter().map(String::as_str)),
        INK,
    );
}

/// Draws pre-split lines as individual single-line labels.
///
/// Kept over one `TextBlockModel` only because the press log grows a line at
/// a time and each entry is independent; there is no longer a rendering
/// reason for it. This started life as a workaround for a Linux bug that
/// ate a text block's leading lines — fixed in `linux.rs`, so the notes
/// panel below is a plain text block again.
fn paint_lines<'a>(
    scene: &mut Vec<PaintOp>,
    bounds: Rect,
    lines: impl Iterator<Item = &'a str>,
    color: Color,
) {
    let line_height = ui_core::single_line_text_box_height(CAPTION_FONT);
    for (index, line) in lines.enumerate() {
        let y = bounds.y + line_height * index as f32;
        if line.is_empty() || y + line_height > bounds.y + bounds.height {
            continue;
        }
        let mut label = LabelModel::new(
            line,
            Rect {
                x: bounds.x,
                y,
                width: bounds.width,
                height: line_height,
            },
        );
        label.style.font_size = CAPTION_FONT;
        label.style.color = color;
        label.paint(scene);
    }
}

fn paint_pads(
    scene: &mut Vec<PaintOp>,
    width: f32,
    height: f32,
    pads: &[GamepadSnapshot],
    seen_any_pad: bool,
) {
    let column = Rect {
        x: OUTER_GUTTER + NOTES_WIDTH + PANEL_GAP,
        y: OUTER_GUTTER,
        width: width - OUTER_GUTTER * 2.0 - NOTES_WIDTH - PANEL_GAP,
        height: height - OUTER_GUTTER * 2.0,
    };

    if pads.is_empty() {
        let mut empty = PanelModel::new(Rect {
            height: CARD_HEIGHT,
            ..column
        });
        empty.background = Some(PANEL_FILL);
        empty.border = Some(PANEL_EDGE);
        empty.padding = PANEL_PADDING;
        empty.paint(scene);
        let message = if seen_any_pad {
            "No gamepad connected now — one was seen earlier."
        } else {
            "No gamepad connected.\n\
             Plug one in; discovery is polled, so it\n\
             may take a second to appear."
        };
        let mut label = TextBlockModel::new(message, empty.content_rect());
        label.style.font_size = BODY_FONT;
        label.style.color = DIM_INK;
        label.paint(scene);
        return;
    }

    for (index, pad) in pads.iter().enumerate() {
        let offset = (CARD_HEIGHT + CARD_GAP) * index as f32;
        if offset + CARD_HEIGHT > column.height {
            break;
        }
        paint_pad_card(
            scene,
            Rect {
                y: column.y + offset,
                height: CARD_HEIGHT,
                ..column
            },
            pad,
        );
    }
}

fn paint_pad_card(scene: &mut Vec<PaintOp>, bounds: Rect, pad: &GamepadSnapshot) {
    let mut panel = PanelModel::new(bounds);
    panel.background = Some(PANEL_FILL);
    panel.border = Some(if pad.connected {
        PANEL_EDGE
    } else {
        Color::rgba(0x8c, 0x50, 0x50, 0xff)
    });
    panel.padding = PANEL_PADDING;
    panel.paint(scene);

    let content = panel.content_rect();
    let header_height = ui_core::single_line_text_box_height(BODY_FONT);
    let mut header = LabelModel::new(
        format!(
            "Pad {} — {}",
            pad.id,
            if pad.connected {
                "connected"
            } else {
                "disconnected"
            }
        ),
        Rect {
            height: header_height,
            ..content
        },
    );
    header.style.font_size = BODY_FONT;
    header.style.vertical_align = VerticalAlign::Middle;
    header.style.color = INK;
    header.paint(scene);

    let body_top = content.y + header_height + 10.0;
    // Spaced by the caption width, not the circle width, so the two
    // readouts cannot collide.
    let stick_column = STICK_RADIUS * 2.8 + 16.0;
    let mut cursor = content.x + STICK_RADIUS * 0.4;
    for (stick, name) in [
        (pad.left_stick, "Left stick"),
        (pad.right_stick, "Right stick"),
    ] {
        paint_stick(
            scene,
            Point {
                x: cursor + STICK_RADIUS,
                y: body_top + STICK_RADIUS,
            },
            stick.raw,
            stick.with_deadzone(DISPLAY_DEADZONE),
            name,
        );
        cursor += stick_column;
    }
    cursor += 12.0;

    // Narrow enough to leave the chip grid room for its longest label:
    // a clipped chip name is exactly the kind of thing this harness is
    // supposed to detect, not exhibit.
    let trigger_width = 118.0;
    for (index, (trigger, name)) in [
        (pad.left_trigger.raw, "L trigger"),
        (pad.right_trigger.raw, "R trigger"),
    ]
    .into_iter()
    .enumerate()
    {
        paint_trigger(
            scene,
            Rect {
                x: cursor,
                y: body_top + (index as f32) * 54.0,
                width: trigger_width,
                height: 40.0,
            },
            trigger,
            name,
        );
    }

    let chips_left = cursor + trigger_width + 18.0;
    paint_button_chips(
        scene,
        Rect {
            x: chips_left,
            y: body_top,
            width: content.x + content.width - chips_left,
            height: content.y + content.height - body_top,
        },
        pad,
    );
}

/// Draws one stick as a dot inside its travel circle, plus the numeric
/// value — the picture catches an inverted axis at a glance, the numbers
/// catch a scaling error the picture would hide.
fn paint_stick(scene: &mut Vec<PaintOp>, center: Point, raw: PointF, shaped: PointF, name: &str) {
    scene.push(PaintOp::StrokeCircle {
        center,
        radius: STICK_RADIUS,
        color: PANEL_EDGE,
        thickness: 1,
    });
    scene.push(PaintOp::Line {
        from: Point {
            x: center.x - STICK_RADIUS,
            y: center.y,
        },
        to: Point {
            x: center.x + STICK_RADIUS,
            y: center.y,
        },
        color: IDLE_CHIP,
    });
    scene.push(PaintOp::Line {
        from: Point {
            x: center.x,
            y: center.y - STICK_RADIUS,
        },
        to: Point {
            x: center.x,
            y: center.y + STICK_RADIUS,
        },
        color: IDLE_CHIP,
    });
    scene.push(PaintOp::FillCircle {
        center: Point {
            x: center.x + raw.x * STICK_RADIUS,
            y: center.y + raw.y * STICK_RADIUS,
        },
        radius: 7.0,
        color: if shaped.x == 0.0 && shaped.y == 0.0 {
            DIM_INK
        } else {
            LIVE_CHIP
        },
    });

    let caption_height = ui_core::single_line_text_box_height(CAPTION_FONT);
    let mut caption = TextBlockModel::new(
        format!(
            "{name}\nraw {:+.2}, {:+.2}\ndz  {:+.2}, {:+.2}",
            raw.x, raw.y, shaped.x, shaped.y
        ),
        Rect {
            // Wider than the circle it labels: a clipped number would read
            // as a wrong value, which is the one thing this must not do.
            x: center.x - STICK_RADIUS * 1.4,
            y: center.y + STICK_RADIUS + 6.0,
            width: STICK_RADIUS * 2.8,
            height: caption_height * 3.0,
        },
    );
    caption.style.font_size = CAPTION_FONT;
    caption.style.horizontal_align = HorizontalAlign::Center;
    caption.style.color = DIM_INK;
    caption.paint(scene);
}

fn paint_trigger(scene: &mut Vec<PaintOp>, bounds: Rect, value: f32, name: &str) {
    let caption_height = ui_core::single_line_text_box_height(CAPTION_FONT);
    let mut caption = LabelModel::new(
        format!("{name}  {value:.2}"),
        Rect {
            height: caption_height,
            ..bounds
        },
    );
    caption.style.font_size = CAPTION_FONT;
    caption.style.color = DIM_INK;
    caption.paint(scene);

    let track = Rect {
        x: bounds.x,
        y: bounds.y + caption_height + 4.0,
        width: bounds.width,
        height: bounds.height - caption_height - 4.0,
    };
    scene.push(PaintOp::FillRect {
        rect: track,
        color: IDLE_CHIP,
    });
    scene.push(PaintOp::FillRect {
        rect: Rect {
            width: track.width * value.clamp(0.0, 1.0),
            ..track
        },
        color: LIVE_CHIP,
    });
    scene.push(PaintOp::StrokeRect {
        rect: track,
        color: PANEL_EDGE,
    });
}

fn paint_button_chips(scene: &mut Vec<PaintOp>, bounds: Rect, pad: &GamepadSnapshot) {
    let columns = 4.0;
    let gap = 6.0;
    let chip_width = ((bounds.width - gap * (columns - 1.0)) / columns).max(10.0);
    let chip_height = 30.0;

    for (index, (button, name)) in BUTTON_ORDER.into_iter().enumerate() {
        let column = (index % 4) as f32;
        let row = (index / 4) as f32;
        let rect = Rect {
            x: bounds.x + column * (chip_width + gap),
            y: bounds.y + row * (chip_height + gap),
            width: chip_width,
            height: chip_height,
        };
        if rect.y + rect.height > bounds.y + bounds.height {
            break;
        }
        let fill = if pad.buttons_pressed.contains(&button) {
            EDGE_CHIP
        } else if pad.buttons_down.contains(&button) {
            LIVE_CHIP
        } else {
            IDLE_CHIP
        };
        scene.push(PaintOp::FillRect { rect, color: fill });
        scene.push(PaintOp::StrokeRect {
            rect,
            color: PANEL_EDGE,
        });
        let mut label = LabelModel::new(name, rect);
        label.style.font_size = CAPTION_FONT;
        label.style.horizontal_align = HorizontalAlign::Center;
        label.style.vertical_align = VerticalAlign::Middle;
        label.style.color = INK;
        label.paint(scene);
    }
}
