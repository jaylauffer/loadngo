use crate::{
    geometry::{Color, Rect},
    input::{Key, PointerButton, UiEvent},
    paint::{PaintOp, TextStyle},
    widget::{WidgetId, WidgetResponse},
};

#[derive(Debug, Clone, PartialEq)]
pub struct SliderModel {
    pub widget_id: WidgetId,
    pub bounds: Rect,
    pub text: String,
    pub min: f32,
    pub max: f32,
    pub value: f32,
    pub hover: bool,
    pub dragging: bool,
    active_pointer: Option<u64>,
    pub focused: bool,
}

impl SliderModel {
    pub fn new(text: impl Into<String>, bounds: Rect, min: f32, max: f32, value: f32) -> Self {
        Self::with_id(WidgetId(0), text, bounds, min, max, value)
    }

    pub fn with_id(
        widget_id: WidgetId,
        text: impl Into<String>,
        bounds: Rect,
        min: f32,
        max: f32,
        value: f32,
    ) -> Self {
        let mut model = Self {
            widget_id,
            bounds,
            text: text.into(),
            min,
            max,
            value,
            hover: false,
            dragging: false,
            active_pointer: None,
            focused: false,
        };
        model.clamp_value();
        model
    }

    pub fn set_bounds(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    pub fn set_value(&mut self, value: f32) {
        self.value = value;
        self.clamp_value();
    }

    pub fn set_range(&mut self, min: f32, max: f32) {
        self.min = min;
        self.max = max;
        self.clamp_value();
    }

    pub fn handle_event(&mut self, event: UiEvent) -> WidgetResponse {
        match event {
            UiEvent::PointerMoved(state) => {
                let hover = self.hit_rect().contains(state.position);
                let mut response = WidgetResponse::default();
                if hover != self.hover {
                    self.hover = hover;
                    response.request_redraw = true;
                }
                if self.dragging
                    && self.active_pointer_matches(state.id)
                    && self.update_from_x(state.position.x)
                {
                    response.request_redraw = true;
                    response.input_consumed = true;
                }
                response
            }
            UiEvent::PointerLeft => {
                if self.hover && !self.dragging {
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
                    self.dragging = true;
                    self.active_pointer = Some(state.id);
                    self.hover = true;
                    let _ = self.update_from_x(state.position.x);
                    return WidgetResponse::redraw_consumed();
                }
                WidgetResponse::default()
            }
            UiEvent::PointerReleased {
                button: PointerButton::Primary,
                state,
            } => {
                if self.dragging && self.active_pointer_matches(state.id) {
                    let _ = self.update_from_x(state.position.x);
                    self.dragging = false;
                    self.active_pointer = None;
                    self.hover = self.hit_rect().contains(state.position);
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
            UiEvent::KeyPressed { key: Key::Left, .. } => {
                if self.adjust_by_step(-1.0) {
                    WidgetResponse::redraw_consumed()
                } else {
                    WidgetResponse::default()
                }
            }
            UiEvent::KeyPressed {
                key: Key::Right, ..
            } => {
                if self.adjust_by_step(1.0) {
                    WidgetResponse::redraw_consumed()
                } else {
                    WidgetResponse::default()
                }
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
        let track = self.track_rect();
        let knob = self.knob_rect();
        let active_width = (track.width * self.normalized_value()).clamp(0.0, track.width);
        let fill = if self.dragging {
            Color::rgba(0x5c, 0x8d, 0xe8, 0xe6)
        } else if self.hover || self.focused {
            Color::rgba(0x6f, 0xa2, 0xf3, 0xd8)
        } else {
            Color::rgba(0x5c, 0x8d, 0xe8, 0xc8)
        };
        let knob_fill = if self.dragging {
            Color::rgba(0xf2, 0xf5, 0xff, 0xf0)
        } else {
            Color::rgba(0xdb, 0xe4, 0xf6, 0xe6)
        };

        scene.push(PaintOp::Text {
            rect: label_rect,
            text: self.text.clone(),
            style: TextStyle {
                color: Color::rgba(0xeb, 0xeb, 0xf5, 0xff),
                font_size: 18,
                centered: false,
            },
        });
        scene.push(PaintOp::FillRect {
            rect: track,
            color: Color::rgba(0x19, 0x1b, 0x23, 0xdc),
        });
        scene.push(PaintOp::FillRect {
            rect: Rect {
                x: track.x,
                y: track.y,
                width: active_width,
                height: track.height,
            },
            color: fill,
        });
        scene.push(PaintOp::StrokeRect {
            rect: track,
            color: Color::rgba(0xa5, 0xaa, 0xbe, 0xeb),
        });
        scene.push(PaintOp::FillRect {
            rect: knob,
            color: knob_fill,
        });
        scene.push(PaintOp::StrokeRect {
            rect: knob,
            color: Color::rgba(0x4d, 0x68, 0x9a, 0xf0),
        });
    }

    fn clamp_value(&mut self) {
        let (min, max) = self.ordered_range();
        self.value = self.value.clamp(min, max);
    }

    fn ordered_range(&self) -> (f32, f32) {
        if self.min <= self.max {
            (self.min, self.max)
        } else {
            (self.max, self.min)
        }
    }

    fn normalized_value(&self) -> f32 {
        let (min, max) = self.ordered_range();
        let span = (max - min).max(f32::EPSILON);
        ((self.value - min) / span).clamp(0.0, 1.0)
    }

    fn value_from_x(&self, x: f32) -> f32 {
        let track = self.track_rect();
        let t = ((x - track.x) / track.width.max(f32::EPSILON)).clamp(0.0, 1.0);
        let (min, max) = self.ordered_range();
        min + t * (max - min)
    }

    fn update_from_x(&mut self, x: f32) -> bool {
        let next = self.value_from_x(x);
        if (next - self.value).abs() > f32::EPSILON {
            self.value = next;
            true
        } else {
            false
        }
    }

    fn adjust_by_step(&mut self, direction: f32) -> bool {
        let (min, max) = self.ordered_range();
        let step = ((max - min) / 100.0).max(0.01);
        let next = (self.value + direction * step).clamp(min, max);
        if (next - self.value).abs() > f32::EPSILON {
            self.value = next;
            true
        } else {
            false
        }
    }

    fn active_pointer_matches(&self, id: u64) -> bool {
        self.active_pointer == Some(id)
    }

    fn hit_rect(&self) -> Rect {
        Rect {
            x: self.bounds.x - 6.0,
            y: self.bounds.y + 8.0,
            width: self.bounds.width + 12.0,
            height: self.bounds.height - 8.0,
        }
    }

    fn track_rect(&self) -> Rect {
        Rect {
            x: self.bounds.x,
            y: self.bounds.y + 22.0,
            width: self.bounds.width,
            height: 12.0,
        }
    }

    fn knob_rect(&self) -> Rect {
        let track = self.track_rect();
        let center_x = track.x + track.width * self.normalized_value();
        Rect {
            x: center_x - 5.0,
            y: track.y - 4.0,
            width: 10.0,
            height: track.height + 8.0,
        }
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

    use super::SliderModel;

    fn pointer(x: f32, y: f32) -> PointerState {
        PointerState::mouse(Point { x, y }, Modifiers::default())
    }

    #[test]
    fn slider_press_and_drag_updates_value() {
        let mut slider = SliderModel::with_id(
            WidgetId(11),
            "Master: 0.50",
            Rect {
                x: 10.0,
                y: 20.0,
                width: 200.0,
                height: 40.0,
            },
            0.0,
            1.0,
            0.5,
        );

        let pressed = slider.handle_event(UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state: pointer(10.0, 44.0),
        });
        assert!(pressed.input_consumed);
        assert!(slider.dragging);
        assert!((slider.value - 0.0).abs() < 0.001);

        let moved = slider.handle_event(UiEvent::PointerMoved(pointer(210.0, 44.0)));
        assert!(moved.input_consumed);
        assert!((slider.value - 1.0).abs() < 0.001);
    }

    #[test]
    fn slider_release_outside_clamps_and_consumes() {
        let mut slider = SliderModel::new(
            "Master: 0.50",
            Rect {
                x: 10.0,
                y: 20.0,
                width: 200.0,
                height: 40.0,
            },
            0.0,
            1.0,
            0.5,
        );

        let _ = slider.handle_event(UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state: pointer(100.0, 44.0),
        });
        let released = slider.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: pointer(400.0, 44.0),
        });
        assert!(released.input_consumed);
        assert!(!slider.dragging);
        assert!((slider.value - 1.0).abs() < 0.001);
    }

    #[test]
    fn slider_arrow_keys_adjust_value() {
        let mut slider = SliderModel::new(
            "Music: 0.50",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 160.0,
                height: 40.0,
            },
            0.0,
            1.0,
            0.5,
        );
        let before = slider.value;
        let response = slider.handle_event(UiEvent::KeyPressed {
            key: crate::input::Key::Right,
            modifiers: Modifiers::default(),
        });
        assert!(response.input_consumed);
        assert!(slider.value > before);
    }

    #[test]
    fn slider_paint_emits_label_track_and_knob() {
        let slider = SliderModel::new(
            "Voice: 0.50",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 160.0,
                height: 40.0,
            },
            0.0,
            1.0,
            0.5,
        );
        let mut scene = Vec::new();
        slider.paint(&mut scene);

        assert!(scene.iter().any(|op| matches!(op, PaintOp::Text { .. })));
        assert!(
            scene
                .iter()
                .filter(|op| matches!(op, PaintOp::FillRect { .. }))
                .count()
                >= 3
        );
        assert!(
            scene
                .iter()
                .filter(|op| matches!(op, PaintOp::StrokeRect { .. }))
                .count()
                >= 2
        );
    }
}
