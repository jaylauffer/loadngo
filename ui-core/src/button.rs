use crate::{
    component::Component,
    geometry::{Color, Rect},
    input::{Key, PointerButton, UiEvent},
    paint::{PaintOp, TextStyle},
    widget::{WidgetId, WidgetResponse},
};

#[derive(Debug, Clone, PartialEq)]
pub struct ButtonModel {
    pub widget_id: WidgetId,
    pub bounds: Rect,
    pub text: String,
    pub hover: bool,
    pub pressed: bool,
    pub focused: bool,
}

impl ButtonModel {
    pub fn new(text: impl Into<String>, bounds: Rect) -> Self {
        Self::with_id(WidgetId(0), text, bounds)
    }

    pub fn with_id(widget_id: WidgetId, text: impl Into<String>, bounds: Rect) -> Self {
        Self {
            widget_id,
            bounds,
            text: text.into(),
            hover: false,
            pressed: false,
            focused: false,
        }
    }

    pub fn set_bounds(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    pub fn handle_event(&mut self, event: UiEvent) -> WidgetResponse {
        match event {
            UiEvent::PointerMoved(state) => {
                let hover = self.bounds.contains(state.position);
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
                if self.bounds.contains(state.position) {
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
                if was_pressed && self.bounds.contains(state.position) {
                    return WidgetResponse::activate(self.widget_id);
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
            UiEvent::KeyPressed {
                key: Key::Enter | Key::Space,
                ..
            } => WidgetResponse::activate(self.widget_id),
            _ => WidgetResponse::default(),
        }
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        let fill = if self.pressed {
            Color::rgba(0xd4, 0xdd, 0xef, 0xf2)
        } else if self.hover || self.focused {
            Color::rgba(0xe8, 0xee, 0xf8, 0xec)
        } else {
            Color::rgba(0xf0, 0xf0, 0xf0, 0xd8)
        };
        let border = if self.pressed {
            Color::rgba(0x4d, 0x68, 0x9a, 0xf0)
        } else if self.hover || self.focused {
            Color::rgba(0x70, 0x70, 0x70, 0xdc)
        } else {
            Color::rgba(0x86, 0x8d, 0xa0, 0xd4)
        };
        scene.push(PaintOp::FillRect {
            rect: self.bounds,
            color: fill,
        });
        scene.push(PaintOp::StrokeRect {
            rect: self.bounds,
            color: border,
        });
        scene.push(PaintOp::Text {
            rect: self.bounds,
            text: self.text.clone(),
            style: TextStyle {
                centered: true,
                ..TextStyle::default()
            },
        });
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Button {
    pub id: i32,
    pub model: ButtonModel,
}

impl Button {
    pub fn new(id: i32, text: impl Into<String>, bounds: Rect) -> Self {
        Self {
            id,
            model: ButtonModel::with_id(WidgetId(id as u64), text, bounds),
        }
    }

    pub fn handle_event(&mut self, event: UiEvent) -> WidgetResponse {
        self.model.handle_event(event)
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        self.model.paint(scene);
    }
}

impl Component for Button {
    fn bounds(&self) -> Rect {
        self.model.bounds
    }

    fn set_bounds(&mut self, rect: Rect) {
        self.model.set_bounds(rect);
    }

    fn focus_changed(&mut self, gained: bool) {
        let _ = self.model.handle_event(UiEvent::FocusChanged(gained));
    }

    fn mouse_entered(&mut self) {
        let position = self.model.bounds;
        let _ = self
            .model
            .handle_event(UiEvent::PointerMoved(crate::input::PointerState::mouse(
                crate::geometry::Point {
                    x: position.x,
                    y: position.y,
                },
                Default::default(),
            )));
    }

    fn mouse_exited(&mut self) {
        let _ = self.model.handle_event(UiEvent::PointerLeft);
    }

    fn id(&self) -> i32 {
        self.id
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        geometry::{Point, Rect},
        input::{Modifiers, PointerButton, PointerState, UiEvent},
        paint::PaintOp,
    };

    use super::ButtonModel;
    use crate::widget::{WidgetAction, WidgetId};

    fn pointer(x: f32, y: f32) -> PointerState {
        PointerState::mouse(Point { x, y }, Modifiers::default())
    }

    #[test]
    fn button_click_emits_activation() {
        let mut button = ButtonModel::with_id(
            WidgetId(7),
            "Run",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 80.0,
                height: 24.0,
            },
        );

        assert!(
            button
                .handle_event(UiEvent::PointerPressed {
                    button: PointerButton::Primary,
                    state: pointer(5.0, 5.0),
                })
                .input_consumed
        );

        let response = button.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: pointer(5.0, 5.0),
        });

        assert_eq!(response.action, Some(WidgetAction::Activate(WidgetId(7))));
        assert!(response.input_consumed);
    }

    #[test]
    fn paint_emits_text_and_background() {
        let button = ButtonModel::new(
            "Run",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 80.0,
                height: 24.0,
            },
        );
        let mut scene = Vec::new();

        button.paint(&mut scene);

        assert!(scene
            .iter()
            .any(|op| matches!(op, PaintOp::FillRect { .. })));
        assert!(scene
            .iter()
            .any(|op| matches!(op, PaintOp::StrokeRect { .. })));
        assert!(scene.iter().any(|op| matches!(op, PaintOp::Text { .. })));
    }

    #[test]
    fn button_release_outside_consumes_press_without_activation() {
        let mut button = ButtonModel::with_id(
            WidgetId(9),
            "Run",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 80.0,
                height: 24.0,
            },
        );
        let press = button.handle_event(UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state: pointer(5.0, 5.0),
        });
        assert!(press.input_consumed);

        let release = button.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: pointer(200.0, 200.0),
        });
        assert!(release.request_redraw);
        assert!(release.input_consumed);
        assert_eq!(release.action, None);
    }
}
