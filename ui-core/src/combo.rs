use crate::{
    component::Component,
    geometry::{Color, Point, Rect},
    input::{Key, UiEvent},
    paint::{HorizontalAlign, PaintOp, TextLayoutMode, TextStyle, VerticalAlign},
    widget::WidgetResponse,
};

#[derive(Debug, Clone, PartialEq)]
pub struct ListCombo {
    pub bounds: Rect,
    pub items: Vec<String>,
    pub selected_index: Option<usize>,
}

impl ListCombo {
    pub fn new(bounds: Rect) -> Self {
        Self {
            bounds,
            items: Vec::new(),
            selected_index: None,
        }
    }

    pub fn add_item(&mut self, text: impl Into<String>) {
        self.items.push(text.into());
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.selected_index = None;
    }

    pub fn select(&mut self, index: usize) -> bool {
        if index < self.items.len() {
            self.selected_index = Some(index);
            true
        } else {
            false
        }
    }

    pub fn selected_item(&self) -> Option<&str> {
        self.selected_index
            .and_then(|index| self.items.get(index))
            .map(String::as_str)
    }

    pub fn handle_event(&mut self, event: UiEvent) -> WidgetResponse {
        match event {
            UiEvent::KeyPressed { key: Key::Down, .. } => {
                if self.items.is_empty() {
                    return WidgetResponse::default();
                }
                let next = self
                    .selected_index
                    .map(|idx| (idx + 1).min(self.items.len() - 1))
                    .unwrap_or(0);
                if self.select(next) {
                    return WidgetResponse::redraw();
                }
            }
            UiEvent::KeyPressed { key: Key::Up, .. } => {
                if self.items.is_empty() {
                    return WidgetResponse::default();
                }
                let next = self
                    .selected_index
                    .map(|idx| idx.saturating_sub(1))
                    .unwrap_or(0);
                if self.select(next) {
                    return WidgetResponse::redraw();
                }
            }
            UiEvent::PointerReleased { state, .. } => {
                if self.bounds.contains(state.position) && !self.items.is_empty() {
                    let next = self
                        .selected_index
                        .map(|idx| (idx + 1) % self.items.len())
                        .unwrap_or(0);
                    if self.select(next) {
                        return WidgetResponse::redraw();
                    }
                }
            }
            _ => {}
        }

        WidgetResponse::default()
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        scene.push(PaintOp::FillRect {
            rect: self.bounds,
            color: Color::rgba(0xf7, 0xf4, 0xea, 0xff),
        });
        scene.push(PaintOp::StrokeRect {
            rect: self.bounds,
            color: Color::rgba(0x76, 0x7b, 0x75, 0xff),
        });
        scene.push(PaintOp::Text {
            rect: Rect {
                x: self.bounds.x + 10.0,
                y: self.bounds.y,
                width: self.bounds.width - 34.0,
                height: self.bounds.height,
            },
            clip_rect: None,
            text: self.selected_item().unwrap_or("Select...").to_string(),
            style: TextStyle {
                horizontal_align: HorizontalAlign::Left,
                vertical_align: VerticalAlign::Middle,
                layout_mode: TextLayoutMode::SingleLine,
                ..TextStyle::default()
            },
        });
        let arrow_x = self.bounds.right() - 18.0;
        let center_y = self.bounds.y + self.bounds.height / 2.0;
        scene.push(PaintOp::Line {
            from: Point {
                x: arrow_x - 6.0,
                y: center_y - 3.0,
            },
            to: Point {
                x: arrow_x,
                y: center_y + 3.0,
            },
            color: Color::rgba(0x55, 0x55, 0x55, 0xff),
        });
        scene.push(PaintOp::Line {
            from: Point {
                x: arrow_x,
                y: center_y + 3.0,
            },
            to: Point {
                x: arrow_x + 6.0,
                y: center_y - 3.0,
            },
            color: Color::rgba(0x55, 0x55, 0x55, 0xff),
        });
    }
}

impl Component for ListCombo {
    fn bounds(&self) -> Rect {
        self.bounds
    }

    fn set_bounds(&mut self, rect: Rect) {
        self.bounds = rect;
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
    use super::ListCombo;
    use crate::{Modifiers, PointerButton, PointerState, Rect, UiEvent};

    #[test]
    fn selecting_item_tracks_selected_text() {
        let mut combo = ListCombo::new(Rect {
            x: 0.0,
            y: 0.0,
            width: 120.0,
            height: 24.0,
        });
        combo.add_item("alpha");
        combo.add_item("beta");

        assert!(combo.select(1));
        assert_eq!(combo.selected_item(), Some("beta"));
        assert!(!combo.select(2));
    }

    #[test]
    fn combo_pointer_release_cycles_selection() {
        let mut combo = ListCombo::new(Rect {
            x: 0.0,
            y: 0.0,
            width: 120.0,
            height: 24.0,
        });
        combo.add_item("alpha");
        combo.add_item("beta");

        let response = combo.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: PointerState::mouse(crate::Point { x: 5.0, y: 5.0 }, Modifiers::default()),
        });

        assert!(response.request_redraw);
        assert_eq!(combo.selected_item(), Some("alpha"));
    }
}
