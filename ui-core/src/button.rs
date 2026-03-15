use crate::{
    component::Component,
    geometry::{Color, Rect},
    input::{Key, PointerButton, UiEvent},
    paint::{PaintOp, TextStyle},
    widget::{WidgetId, WidgetResponse},
};

#[derive(Debug, Clone, PartialEq, Eq)]
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
                    return WidgetResponse::redraw();
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
                    return WidgetResponse::redraw();
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
        scene.push(PaintOp::FillRect {
            rect: self.bounds,
            color: Color::rgba(0xf0, 0xf0, 0xf0, 0xd8),
        });
        scene.push(PaintOp::Text {
            rect: self.bounds,
            text: self.text.clone(),
            style: TextStyle {
                centered: true,
                ..TextStyle::default()
            },
        });
        if self.hover || self.focused {
            scene.push(PaintOp::StrokeRect {
                rect: self.bounds,
                color: Color::rgba(0x70, 0x70, 0x70, 0xdc),
            });
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

    fn pointer(x: i32, y: i32) -> PointerState {
        PointerState::mouse(Point { x, y }, Modifiers::default())
    }

    #[test]
    fn button_click_emits_activation() {
        let mut button = ButtonModel::with_id(
            WidgetId(7),
            "Run",
            Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 24,
            },
        );

        assert!(
            button
                .handle_event(UiEvent::PointerPressed {
                    button: PointerButton::Primary,
                    state: pointer(5, 5),
                })
                .request_redraw
        );

        let response = button.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: pointer(5, 5),
        });

        assert_eq!(response.action, Some(WidgetAction::Activate(WidgetId(7))));
    }

    #[test]
    fn paint_emits_text_and_background() {
        let button = ButtonModel::new(
            "Run",
            Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 24,
            },
        );
        let mut scene = Vec::new();

        button.paint(&mut scene);

        assert!(matches!(scene[0], PaintOp::FillRect { .. }));
        assert!(matches!(scene[1], PaintOp::Text { .. }));
    }
}
