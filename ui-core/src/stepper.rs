use crate::{
    geometry::{Color, Rect},
    input::{PointerButton, UiEvent},
    paint::{PaintOp, TextStyle},
    widget::{WidgetId, WidgetResponse},
};

#[derive(Debug, Clone, PartialEq)]
pub struct StepperModel {
    pub widget_id: WidgetId,
    pub bounds: Rect,
    pub text: String,
    pub value_text: String,
    pub focused: bool,
    pressed_prev: bool,
    pressed_next: bool,
    pending_delta: i32,
}

impl StepperModel {
    pub fn new(text: impl Into<String>, value_text: impl Into<String>, bounds: Rect) -> Self {
        Self::with_id(WidgetId(0), text, value_text, bounds)
    }

    pub fn with_id(
        widget_id: WidgetId,
        text: impl Into<String>,
        value_text: impl Into<String>,
        bounds: Rect,
    ) -> Self {
        Self {
            widget_id,
            bounds,
            text: text.into(),
            value_text: value_text.into(),
            focused: false,
            pressed_prev: false,
            pressed_next: false,
            pending_delta: 0,
        }
    }

    pub fn set_bounds(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    pub fn set_value_text(&mut self, value_text: impl Into<String>) {
        self.value_text = value_text.into();
    }

    pub fn take_pending_delta(&mut self) -> i32 {
        let delta = self.pending_delta;
        self.pending_delta = 0;
        delta
    }

    pub fn handle_event(&mut self, event: UiEvent) -> WidgetResponse {
        match event {
            UiEvent::PointerPressed {
                button: PointerButton::Primary,
                state,
            } => {
                if self.prev_rect().contains(state.position) {
                    self.pressed_prev = true;
                    return WidgetResponse::redraw_consumed();
                }
                if self.next_rect().contains(state.position) {
                    self.pressed_next = true;
                    return WidgetResponse::redraw_consumed();
                }
                WidgetResponse::default()
            }
            UiEvent::PointerReleased {
                button: PointerButton::Primary,
                state,
            } => {
                let mut response = WidgetResponse::default();
                if self.pressed_prev {
                    self.pressed_prev = false;
                    response = WidgetResponse::redraw_consumed();
                    if self.prev_rect().contains(state.position) {
                        self.pending_delta -= 1;
                    }
                }
                if self.pressed_next {
                    self.pressed_next = false;
                    response = WidgetResponse::redraw_consumed();
                    if self.next_rect().contains(state.position) {
                        self.pending_delta += 1;
                    }
                }
                response
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
        let label_rect = Rect {
            x: self.bounds.x,
            y: self.bounds.y,
            width: self.bounds.width,
            height: 18.0,
        };
        let button_y = self.bounds.y + 24.0;
        let button_h = (self.bounds.height - 24.0).max(30.0);

        scene.push(PaintOp::Text {
            rect: label_rect,
            text: self.text.clone(),
            style: TextStyle {
                color: Color::rgba(235, 235, 245, 255),
                font_size: 18,
                centered: false,
            },
        });

        self.paint_button(
            scene,
            self.prev_rect_with(button_y, button_h),
            "<",
            self.pressed_prev,
        );
        scene.push(PaintOp::FillRect {
            rect: self.value_rect_with(button_y, button_h),
            color: Color::rgba(24, 30, 46, 220),
        });
        scene.push(PaintOp::StrokeRect {
            rect: self.value_rect_with(button_y, button_h),
            color: Color::rgba(170, 180, 205, 230),
        });
        scene.push(PaintOp::Text {
            rect: self.value_rect_with(button_y, button_h),
            text: self.value_text.clone(),
            style: TextStyle {
                color: Color::rgba(255, 255, 255, 255),
                font_size: 18,
                centered: true,
            },
        });
        self.paint_button(
            scene,
            self.next_rect_with(button_y, button_h),
            ">",
            self.pressed_next,
        );
    }

    fn paint_button(&self, scene: &mut Vec<PaintOp>, rect: Rect, label: &str, pressed: bool) {
        let fill = if pressed {
            Color::rgba(92, 141, 232, 230)
        } else {
            Color::rgba(24, 30, 46, 220)
        };
        scene.push(PaintOp::FillRect { rect, color: fill });
        scene.push(PaintOp::StrokeRect {
            rect,
            color: Color::rgba(170, 180, 205, 230),
        });
        scene.push(PaintOp::Text {
            rect,
            text: label.to_string(),
            style: TextStyle {
                color: Color::rgba(255, 255, 255, 255),
                font_size: 18,
                centered: true,
            },
        });
    }

    fn prev_rect(&self) -> Rect {
        self.prev_rect_with(self.bounds.y + 24.0, (self.bounds.height - 24.0).max(30.0))
    }

    fn next_rect(&self) -> Rect {
        self.next_rect_with(self.bounds.y + 24.0, (self.bounds.height - 24.0).max(30.0))
    }

    fn prev_rect_with(&self, y: f32, h: f32) -> Rect {
        Rect {
            x: self.bounds.x,
            y,
            width: 40.0,
            height: h,
        }
    }

    fn value_rect_with(&self, y: f32, h: f32) -> Rect {
        let gap = 8.0;
        let arrow_w = 40.0;
        let value_w = (self.bounds.width - arrow_w * 2.0 - gap * 2.0).max(120.0);
        Rect {
            x: self.bounds.x + arrow_w + gap,
            y,
            width: value_w,
            height: h,
        }
    }

    fn next_rect_with(&self, y: f32, h: f32) -> Rect {
        let value = self.value_rect_with(y, h);
        Rect {
            x: value.x + value.width + 8.0,
            y,
            width: 40.0,
            height: h,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        geometry::{Point, Rect},
        input::{Modifiers, PointerButton, PointerState, UiEvent},
        paint::PaintOp,
    };

    use super::StepperModel;

    fn pointer(x: f32, y: f32) -> PointerState {
        PointerState::mouse(Point { x, y }, Modifiers::default())
    }

    #[test]
    fn stepper_prev_and_next_emit_pending_delta() {
        let mut stepper = StepperModel::new(
            "Camera device",
            "Integrated Camera",
            Rect {
                x: 10.0,
                y: 20.0,
                width: 320.0,
                height: 62.0,
            },
        );
        let _ = stepper.handle_event(UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state: pointer(20.0, 54.0),
        });
        let release_prev = stepper.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: pointer(20.0, 54.0),
        });
        assert!(release_prev.input_consumed);
        assert_eq!(stepper.take_pending_delta(), -1);

        let _ = stepper.handle_event(UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state: pointer(320.0, 54.0),
        });
        let release_next = stepper.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: pointer(320.0, 54.0),
        });
        assert!(release_next.input_consumed);
        assert_eq!(stepper.take_pending_delta(), 1);
    }

    #[test]
    fn stepper_paint_emits_text_and_buttons() {
        let stepper = StepperModel::new(
            "Camera device",
            "Integrated Camera",
            Rect {
                x: 10.0,
                y: 20.0,
                width: 320.0,
                height: 62.0,
            },
        );
        let mut scene = Vec::new();
        stepper.paint(&mut scene);
        assert!(
            scene
                .iter()
                .filter(|op| matches!(op, PaintOp::Text { .. }))
                .count()
                >= 4
        );
        assert!(
            scene
                .iter()
                .filter(|op| matches!(op, PaintOp::FillRect { .. }))
                .count()
                >= 3
        );
    }
}
