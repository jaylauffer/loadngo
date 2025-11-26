use std::time::{Duration, Instant};

use loadngo_host_core::{FrameDemand, HostKey, HostKeyEvent, InputSnapshot, WindowDescriptor};
use ui_core::{
    Color, HorizontalAlign, Insets, Key, LabelModel, PanelModel, Point, PointerButton,
    PointerState, Rect, TextAreaModel, TextBlockModel, UiEvent, VerticalAlign,
};

const WINDOW_WIDTH: i32 = 1440;
const WINDOW_HEIGHT: i32 = 980;
const OUTER_GUTTER: f32 = 24.0;
const PANEL_GAP: f32 = 20.0;
const TITLE_FONT: u16 = 24;
const BODY_FONT: u16 = 18;
const CAPTION_FONT: u16 = 15;
const PANEL_PADDING: Insets = Insets {
    left: 16.0,
    top: 14.0,
    right: 16.0,
    bottom: 16.0,
};

fn main() {
    loadngo_host_desktop::launch(window_descriptor(), None, async move {
        run_text_input_harness().await;
    });
}

fn window_descriptor() -> WindowDescriptor {
    WindowDescriptor {
        title: "loadngo text input harness".to_string(),
        width: Some(WINDOW_WIDTH),
        height: Some(WINDOW_HEIGHT),
        high_dpi: true,
        linux_wm_class: Some("loadngo-text-input-harness"),
    }
}

async fn run_text_input_harness() {
    let mut area = build_text_area(Rect::default());
    let mut caret_blink_origin = Instant::now();

    loop {
        let frame = loadngo_host_desktop::capture_frame();
        if frame.input.key_pressed(HostKey::Escape) {
            loadngo_host_desktop::set_text_cursor_active(false);
            break;
        }

        let layout = HarnessLayout::new(frame.surface.width, frame.surface.height);
        let pointer = Point {
            x: frame.input.mouse_x,
            y: frame.input.mouse_y,
        };
        area.set_bounds(layout.editor);
        area.relayout(measure_width);
        let had_activity = route_input(&mut area, &frame.input);
        if had_activity {
            caret_blink_origin = Instant::now();
        }
        area.show_caret =
            area.focused && ((caret_blink_origin.elapsed().as_millis() / 530) % 2 == 0);
        area.relayout(measure_width);
        loadngo_host_desktop::set_text_cursor_active(
            (area.drag_selecting && area.bounds.contains(pointer))
                || area.prefers_text_cursor(pointer),
        );

        let mut scene = Vec::new();
        paint_shell(&mut scene, &layout);
        area.paint(&mut scene);

        loadngo_host_desktop::clear(Color::rgba(0x10, 0x15, 0x1d, 0xff));
        loadngo_host_desktop::render_widget_paint_ops(&scene);
        loadngo_host_desktop::next_frame(FrameDemand::after(Duration::from_millis(16))).await;
    }
}

fn build_text_area(bounds: Rect) -> TextAreaModel {
    let mut area = TextAreaModel::new(
        "Ops(\n\
    [\n\
        Label(\"start\"),\n\
        Scene(\"bg_tallahassee_dawn.png\"),\n\
        Say(\"Narrator\", \"The script buffer is the source of truth.\"),\n\
        Menu([\n\
            (\"Open fridge\", \"fridge_branch\"),\n\
            (\"Look outside\", \"window_branch\"),\n\
        ]),\n\
        Jump(\"credits\"),\n\
    ],\n\
)\n",
        bounds,
    );
    area.style.font_size = BODY_FONT;
    area.line_spacing = 3.0;
    area.background = Some(Color::rgba(0x12, 0x18, 0x22, 0xf7));
    area.border = Some(Color::rgba(0x6d, 0x7b, 0x93, 0xff));
    area.selection_fill = Color::rgba(0x36, 0x5c, 0x96, 0xd8);
    area.caret_color = Color::rgba(0xf2, 0xf5, 0xfb, 0xff);
    area
}

#[derive(Debug, Clone, Copy)]
struct HarnessLayout {
    left_panel: Rect,
    editor_panel: Rect,
    editor: Rect,
}

impl HarnessLayout {
    fn new(surface_width: f32, surface_height: f32) -> Self {
        let outer = Rect {
            x: OUTER_GUTTER,
            y: OUTER_GUTTER,
            width: (surface_width - OUTER_GUTTER * 2.0).max(720.0),
            height: (surface_height - OUTER_GUTTER * 2.0).max(540.0),
        };
        let left_width = (outer.width * 0.28).max(260.0);
        let gap = PANEL_GAP;
        let editor_width = (outer.width - left_width - gap).max(360.0);
        let editor_panel = Rect {
            x: outer.x + left_width + gap,
            y: outer.y,
            width: editor_width,
            height: outer.height,
        };
        let editor = inset_rect(editor_panel, PANEL_PADDING, TITLE_FONT, 16.0);
        Self {
            left_panel: Rect {
                x: outer.x,
                y: outer.y,
                width: left_width,
                height: outer.height,
            },
            editor_panel,
            editor,
        }
    }
}

fn paint_shell(scene: &mut Vec<ui_core::PaintOp>, layout: &HarnessLayout) {
    let mut notes_panel = PanelModel::new(layout.left_panel);
    notes_panel.background = Some(Color::rgba(0x1a, 0x22, 0x2f, 0xf0));
    notes_panel.border = Some(Color::rgba(0x73, 0x82, 0x9c, 0xff));
    notes_panel.padding = PANEL_PADDING;
    notes_panel.paint(scene);

    let notes_content = notes_panel.content_rect();
    let mut notes_title = LabelModel::new(
        "Text Input Harness",
        Rect {
            x: notes_content.x,
            y: notes_content.y,
            width: notes_content.width,
            height: ui_core::single_line_text_box_height(TITLE_FONT),
        },
    );
    notes_title.style.font_size = TITLE_FONT;
    notes_title.style.vertical_align = VerticalAlign::Middle;
    notes_title.style.color = Color::rgba(0xf4, 0xf7, 0xfb, 0xff);
    notes_title.paint(scene);

    let mut notes = TextBlockModel::new(
        "Purpose\n\
Validate multiline desktop text input independently from the editor.\n\n\
Current scope\n\
- authoritative source buffer\n\
- click to place caret\n\
- drag to select\n\
- arrows/home/end\n\
- backspace/delete/enter/tab\n\
- scroll wheel\n\n\
Shortcuts\n\
Escape closes the window.\n\
Cmd/Ctrl+A selects all text.",
        Rect {
            x: notes_content.x,
            y: notes_content.y + ui_core::single_line_text_box_height(TITLE_FONT) + 12.0,
            width: notes_content.width,
            height: notes_content.height - ui_core::single_line_text_box_height(TITLE_FONT) - 12.0,
        },
    );
    notes.style.font_size = CAPTION_FONT;
    notes.style.color = Color::rgba(0xd8, 0xe1, 0xf0, 0xff);
    notes.paint(scene);

    let mut editor_panel = PanelModel::new(layout.editor_panel);
    editor_panel.background = Some(Color::rgba(0x17, 0x1d, 0x28, 0xf4));
    editor_panel.border = Some(Color::rgba(0x80, 0x8d, 0xa4, 0xff));
    editor_panel.padding = PANEL_PADDING;
    editor_panel.paint(scene);

    let editor_content = editor_panel.content_rect();
    let mut editor_title = LabelModel::new(
        "TextAreaModel",
        Rect {
            x: editor_content.x,
            y: editor_content.y,
            width: editor_content.width,
            height: ui_core::single_line_text_box_height(TITLE_FONT),
        },
    );
    editor_title.style.font_size = TITLE_FONT;
    editor_title.style.color = Color::rgba(0xf3, 0xf7, 0xfc, 0xff);
    editor_title.style.vertical_align = VerticalAlign::Middle;
    editor_title.paint(scene);

    let mut editor_caption = LabelModel::new(
        "First-phase multiline source editing surface for the .ron-first editor plan.",
        Rect {
            x: editor_content.x,
            y: editor_content.y + ui_core::single_line_text_box_height(TITLE_FONT) + 4.0,
            width: editor_content.width,
            height: ui_core::single_line_text_box_height(CAPTION_FONT),
        },
    );
    editor_caption.style.font_size = CAPTION_FONT;
    editor_caption.style.color = Color::rgba(0xc9, 0xd3, 0xe4, 0xff);
    editor_caption.style.vertical_align = VerticalAlign::Middle;
    editor_caption.style.horizontal_align = HorizontalAlign::Left;
    editor_caption.paint(scene);
}

fn route_input(area: &mut TextAreaModel, input: &InputSnapshot) -> bool {
    let mut had_activity = false;
    let pointer = PointerState::mouse(
        Point {
            x: input.mouse_x,
            y: input.mouse_y,
        },
        input.modifiers,
    );

    let pointer_inside = area.bounds.contains(pointer.position);
    if pointer_inside
        || area.drag_selecting
        || area.horizontal_drag.is_some()
        || area.vertical_drag.is_some()
        || area.hover
    {
        let response = area.handle_event(UiEvent::PointerMoved(pointer));
        had_activity |= response.request_redraw || response.input_consumed;
    } else {
        let response = area.handle_event(UiEvent::PointerLeft);
        had_activity |= response.request_redraw || response.input_consumed;
    }

    if input.mouse_pressed {
        if pointer_inside {
            let response = area.handle_event(UiEvent::PointerPressed {
                button: PointerButton::Primary,
                state: pointer,
            });
            had_activity |= response.request_redraw || response.input_consumed;
        } else if area.focused {
            let response = area.handle_event(UiEvent::FocusChanged(false));
            had_activity |= response.request_redraw || response.input_consumed;
        }
    }
    if input.mouse_released {
        let response = area.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: pointer,
        });
        had_activity |= response.request_redraw || response.input_consumed;
    }
    if input.mouse_wheel_x.abs() > f32::EPSILON && pointer_inside {
        let changed = area.scroll_horizontal(-input.mouse_wheel_x * 36.0);
        if changed {
            area.relayout(measure_width);
            had_activity = true;
        }
    }
    if input.mouse_wheel_y.abs() > f32::EPSILON && pointer_inside {
        let delta = input.mouse_wheel_y.round() as i32;
        if delta != 0 {
            let response = if input.modifiers.shift {
                let changed = area.scroll_horizontal(-(delta as f32) * 36.0);
                if changed {
                    area.relayout(measure_width);
                    ui_core::WidgetResponse::redraw_consumed()
                } else {
                    ui_core::WidgetResponse::default()
                }
            } else {
                area.handle_event(UiEvent::ScrollLines { delta })
            };
            had_activity |= response.request_redraw || response.input_consumed;
        }
    }
    for key_event in &input.key_events {
        if let Some(key) = key_from_host(key_event) {
            let response = area.handle_event(UiEvent::KeyPressed {
                key,
                modifiers: key_event.modifiers,
            });
            had_activity |= response.request_redraw || response.input_consumed;
        }
    }
    if !input.typed_text.is_empty() {
        let response = area.handle_event(UiEvent::TextInput {
            text: input.typed_text.clone(),
        });
        had_activity |= response.request_redraw || response.input_consumed;
    }
    had_activity
}

fn key_from_host(event: &HostKeyEvent) -> Option<Key> {
    Some(match event.key {
        HostKey::Enter => Key::Enter,
        HostKey::Space => Key::Space,
        HostKey::Escape => Key::Escape,
        HostKey::Tab => Key::Tab,
        HostKey::Up => Key::Up,
        HostKey::Down => Key::Down,
        HostKey::Left => Key::Left,
        HostKey::Right => Key::Right,
        HostKey::Home => Key::Home,
        HostKey::End => Key::End,
        HostKey::Backspace => Key::Backspace,
        HostKey::Delete => Key::Delete,
        HostKey::A => Key::Character('a'),
        _ => return None,
    })
}

fn measure_width(text: &str, font_size: u16) -> f32 {
    loadngo_host_desktop::measure_text_metrics(text, None, font_size, 1.0).width
}

fn inset_rect(panel: Rect, padding: Insets, title_font: u16, caption_gap: f32) -> Rect {
    let title_height = ui_core::single_line_text_box_height(title_font);
    Rect {
        x: panel.x + padding.left,
        y: panel.y
            + padding.top
            + title_height
            + caption_gap
            + ui_core::single_line_text_box_height(CAPTION_FONT),
        width: (panel.width - padding.left - padding.right).max(0.0),
        height: (panel.height
            - padding.top
            - padding.bottom
            - title_height
            - caption_gap
            - ui_core::single_line_text_box_height(CAPTION_FONT))
        .max(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::{build_text_area, key_from_host, measure_width};
    use loadngo_host_core::{HostKey, HostKeyEvent};
    use ui_core::{Modifiers, Rect, TextAreaModel};

    #[test]
    fn host_a_maps_to_select_all_character_key() {
        let key = key_from_host(&HostKeyEvent {
            key: HostKey::A,
            modifiers: Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
        });
        assert_eq!(key, Some(ui_core::Key::Character('a')));
    }

    #[test]
    fn text_area_relayout_builds_lines_for_seed_text() {
        let mut area: TextAreaModel = build_text_area(Rect {
            x: 0.0,
            y: 0.0,
            width: 640.0,
            height: 480.0,
        });
        area.relayout(measure_width);
        assert!(area.layout_cache.lines.len() > 5);
        assert!(area.layout_cache.content_height > 0.0);
    }
}
