use crate::{
    geometry::{Point, Rect},
    input::UiEvent,
    widget::WidgetResponse,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListInteraction {
    None,
    Selected(usize),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ListState {
    pub item_heights: Vec<f32>,
    pub item_bounds: Vec<Rect>,
    pub hover_index: Option<usize>,
    pub selected_index: Option<usize>,
    pub visible_pos: usize,
    pub visible_count: usize,
    pub content_height: f32,
}

impl ListState {
    pub fn set_item_heights(&mut self, item_heights: Vec<f32>) {
        self.item_heights = item_heights;
        self.item_bounds.clear();
        self.hover_index = None;
        self.selected_index = None;
        self.visible_pos = 0;
        self.visible_count = 0;
        self.content_height = 0.0;
    }

    pub fn layout(&mut self, viewport: Rect, origin_y: f32) {
        self.item_bounds.clear();
        let mut y = origin_y;
        self.visible_count = 0;
        self.content_height = 0.0;

        for height in self.item_heights.iter().skip(self.visible_pos) {
            let rect = Rect {
                x: 0.0,
                y,
                width: viewport.width,
                height: *height,
            };
            self.item_bounds.push(rect);
            y += *height;
            self.content_height += *height;
            if y <= viewport.height {
                self.visible_count += 1;
            }
        }
    }

    pub fn hit_test(&self, point: Point) -> Option<usize> {
        self.item_bounds
            .iter()
            .enumerate()
            .find(|(_, rect)| rect.contains(point))
            .map(|(offset, _)| self.visible_pos + offset)
    }

    pub fn handle_event(&mut self, event: UiEvent) -> (WidgetResponse, ListInteraction) {
        match event {
            UiEvent::PointerMoved(state) => {
                let hover = self.hit_test(state.position);
                if hover != self.hover_index {
                    self.hover_index = hover;
                    return (WidgetResponse::redraw(), ListInteraction::None);
                }
            }
            UiEvent::PointerLeft => {
                if self.hover_index.take().is_some() {
                    return (WidgetResponse::redraw(), ListInteraction::None);
                }
            }
            UiEvent::PointerReleased { state, .. } => {
                let hit = self.hit_test(state.position);
                if hit != self.selected_index {
                    self.selected_index = hit;
                    if let Some(index) = hit {
                        return (WidgetResponse::redraw(), ListInteraction::Selected(index));
                    }
                }
            }
            UiEvent::ScrollLines { delta } => {
                let current = self.visible_pos as i32;
                let max_pos = self.item_heights.len().saturating_sub(1) as i32;
                let next = (current - delta).clamp(0, max_pos) as usize;
                if next != self.visible_pos {
                    self.visible_pos = next;
                    return (WidgetResponse::redraw(), ListInteraction::None);
                }
            }
            _ => {}
        }

        (WidgetResponse::default(), ListInteraction::None)
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        geometry::{Point, Rect},
        input::{Modifiers, PointerButton, PointerState, UiEvent},
    };

    use super::{ListInteraction, ListState};

    fn pointer(x: f32, y: f32) -> PointerState {
        PointerState::mouse(Point { x, y }, Modifiers::default())
    }

    #[test]
    fn layout_and_hit_test_track_visible_items() {
        let mut state = ListState::default();
        state.set_item_heights(vec![20.0, 25.0, 30.0]);
        state.layout(
            Rect {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 120.0,
            },
            5.0,
        );

        assert_eq!(state.visible_count, 3);
        assert_eq!(state.hit_test(Point { x: 5.0, y: 10.0 }), Some(0));
        assert_eq!(state.hit_test(Point { x: 5.0, y: 55.0 }), Some(2));
    }

    #[test]
    fn pointer_release_selects_item() {
        let mut state = ListState::default();
        state.set_item_heights(vec![20.0, 20.0]);
        state.layout(
            Rect {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 120.0,
            },
            0.0,
        );

        let (_, interaction) = state.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: pointer(10.0, 25.0),
        });

        assert_eq!(interaction, ListInteraction::Selected(1));
        assert_eq!(state.selected_index, Some(1));
    }

    #[test]
    fn layout_preserves_fractional_origin_and_content_height() {
        let mut state = ListState::default();
        state.set_item_heights(vec![12.5, 17.25, 8.75]);
        state.layout(
            Rect {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 120.0,
            },
            3.5,
        );

        assert!((state.item_bounds[0].y - 3.5).abs() < f32::EPSILON);
        assert!((state.item_bounds[1].y - 16.0).abs() < 0.001);
        assert!((state.content_height - 38.5).abs() < 0.001);
    }
}
