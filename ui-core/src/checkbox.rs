use crate::{
    geometry::{Color, Rect},
    input::{PointerButton, UiEvent},
    paint::{HorizontalAlign, PaintOp, TextLayoutMode, TextStyle, VerticalAlign},
    widget::{WidgetId, WidgetResponse},
};

#[derive(Debug, Clone, PartialEq)]
pub struct CheckboxModel {
    pub widget_id: WidgetId,
    pub bounds: Rect,
    pub text: String,
    pub value: bool,
    pub hover: bool,
    pub pressed: bool,
    pub focused: bool,
}

impl CheckboxModel {
    pub fn new(text: impl Into<String>, bounds: Rect, value: bool) -> Self {
        Self::with_id(WidgetId(0), text, bounds, value)
    }

    pub fn with_id(
        widget_id: WidgetId,
        text: impl Into<String>,
        bounds: Rect,
        value: bool,
    ) -> Self {
        Self {
            widget_id,
            bounds,
            text: text.into(),
            value,
            hover: false,
            pressed: false,
            focused: false,
        }
    }

    pub fn set_bounds(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    pub fn set_value(&mut self, value: bool) {
        self.value = value;
    }

    pub fn handle_event(&mut self, event: UiEvent) -> WidgetResponse {
        match event {
            UiEvent::PointerMoved(state) => {
                let hover = self.hit_rect().contains(state.position);
                if hover != self.hover {
                    self.hover = hover;
                    return WidgetResponse::redraw();
                }
                WidgetResponse::default()
            }
            UiEvent::PointerLeft => {
                if self.hover {
                    self.hover = false;
                    return WidgetResponse::redraw();
                }
                WidgetResponse::default()
            }
            UiEvent::PointerPressed {
                button: PointerButton::Primary,
                state,
            } => {
                if self.hit_rect().contains(state.position) {
                    self.pressed = true;
                    return WidgetResponse::redraw_consumed();
                }
                WidgetResponse::default()
            }
            UiEvent::PointerReleased {
                button: PointerButton::Primary,
                state,
            } => {
                let was_pressed = self.pressed;
                self.pressed = false;
                if was_pressed && self.hit_rect().contains(state.position) {
                    self.value = !self.value;
                    return WidgetResponse::redraw_consumed();
                }
                if was_pressed {
                    return WidgetResponse::redraw_consumed();
                }
                WidgetResponse::default()
            }
            UiEvent::FocusChanged(focused) => {
                if self.focused != focused {
                    self.focused = focused;
                    return WidgetResponse::redraw();
                }
                WidgetResponse::default()
            }
            _ => WidgetResponse::default(),
        }
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        let box_size = self.bounds.height.min(22.0);
        let box_rect = Rect {
            x: self.bounds.x,
            y: self.bounds.y + ((self.bounds.height - box_size) * 0.5),
            width: box_size,
            height: box_size,
        };
        let border = if self.pressed {
            Color::rgba(0x4d, 0x68, 0x9a, 0xf0)
        } else {
            Color::rgba(0xaa, 0xb4, 0xcd, 0xe6)
        };
        scene.push(PaintOp::FillRect {
            rect: box_rect,
            color: Color::rgba(24, 30, 46, 220),
        });
        scene.push(PaintOp::StrokeRect {
            rect: box_rect,
            color: border,
        });
        if self.value {
            scene.push(PaintOp::FillRect {
                rect: Rect {
                    x: box_rect.x + 4.0,
                    y: box_rect.y + 4.0,
                    width: (box_rect.width - 8.0).max(0.0),
                    height: (box_rect.height - 8.0).max(0.0),
                },
                color: Color::rgba(92, 141, 232, 230),
            });
        }
        scene.push(PaintOp::Text {
            rect: Rect {
                x: self.bounds.x + box_size + 10.0,
                y: self.bounds.y,
                width: (self.bounds.width - box_size - 10.0).max(0.0),
                height: self.bounds.height,
            },
            clip_rect: None,
            text: self.text.clone(),
            style: TextStyle {
                color: Color::rgba(255, 255, 255, 255),
                font_size: 18,
                horizontal_align: HorizontalAlign::Left,
                vertical_align: VerticalAlign::Middle,
                vertical_metric_mode: crate::TextVerticalMetricMode::LogicalLineBox,
                layout_mode: TextLayoutMode::SingleLine,
                overflow: crate::TextOverflow::Clip,
            },
        });
    }

    fn hit_rect(&self) -> Rect {
        self.bounds
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        geometry::{Point, Rect},
        input::{Modifiers, PointerButton, PointerState, UiEvent},
        paint::PaintOp,
        widget::WidgetId,
    };

    use super::CheckboxModel;

    fn pointer(x: f32, y: f32) -> PointerState {
        PointerState::mouse(Point { x, y }, Modifiers::default())
    }

    #[test]
    fn checkbox_click_toggles_value() {
        let mut checkbox = CheckboxModel::with_id(
            WidgetId(1),
            "Enable Voiceover",
            Rect {
                x: 10.0,
                y: 20.0,
                width: 220.0,
                height: 28.0,
            },
            false,
        );
        assert!(
            checkbox
                .handle_event(UiEvent::PointerPressed {
                    button: PointerButton::Primary,
                    state: pointer(20.0, 30.0),
                })
                .input_consumed
        );
        let response = checkbox.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: pointer(20.0, 30.0),
        });
        assert!(response.input_consumed);
        assert!(checkbox.value);
    }

    #[test]
    fn checkbox_release_outside_does_not_toggle() {
        let mut checkbox = CheckboxModel::new(
            "Enable Voiceover",
            Rect {
                x: 10.0,
                y: 20.0,
                width: 220.0,
                height: 28.0,
            },
            false,
        );
        let _ = checkbox.handle_event(UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state: pointer(20.0, 30.0),
        });
        let response = checkbox.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: pointer(400.0, 30.0),
        });
        assert!(response.input_consumed);
        assert!(!checkbox.value);
    }

    #[test]
    fn checkbox_paint_emits_box_and_text() {
        let checkbox = CheckboxModel::new(
            "Enable Voiceover",
            Rect {
                x: 10.0,
                y: 20.0,
                width: 220.0,
                height: 28.0,
            },
            true,
        );
        let mut scene = Vec::new();
        checkbox.paint(&mut scene);
        assert!(scene.iter().any(|op| matches!(op, PaintOp::Text { .. })));
        assert!(
            scene
                .iter()
                .filter(|op| matches!(op, PaintOp::FillRect { .. }))
                .count()
                >= 2
        );
        assert!(scene
            .iter()
            .any(|op| matches!(op, PaintOp::StrokeRect { .. })));
    }
}
