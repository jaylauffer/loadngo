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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ListState {
    pub item_heights: Vec<i32>,
    pub item_bounds: Vec<Rect>,
    pub hover_index: Option<usize>,
    pub selected_index: Option<usize>,
    pub visible_pos: usize,
    pub visible_count: usize,
    pub content_height: i32,
}

impl ListState {
    pub fn set_item_heights(&mut self, item_heights: Vec<i32>) {
        self.item_heights = item_heights;
        self.item_bounds.clear();
        self.hover_index = None;
        self.selected_index = None;
        self.visible_pos = 0;
        self.visible_count = 0;
        self.content_height = 0;
    }

    pub fn layout(&mut self, viewport: Rect, origin_y: i32) {
        self.item_bounds.clear();
        let mut y = origin_y;
        self.visible_count = 0;
        self.content_height = 0;

        for height in self.item_heights.iter().skip(self.visible_pos) {
            let rect = Rect {
                x: 0,
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

    fn pointer(x: i32, y: i32) -> PointerState {
        PointerState::mouse(Point { x, y }, Modifiers::default())
    }

    #[test]
    fn layout_and_hit_test_track_visible_items() {
        let mut state = ListState::default();
        state.set_item_heights(vec![20, 25, 30]);
        state.layout(
            Rect {
                x: 0,
                y: 0,
                width: 120,
                height: 120,
            },
            5,
        );

        assert_eq!(state.visible_count, 3);
        assert_eq!(state.hit_test(Point { x: 5, y: 10 }), Some(0));
        assert_eq!(state.hit_test(Point { x: 5, y: 55 }), Some(2));
    }

    #[test]
    fn pointer_release_selects_item() {
        let mut state = ListState::default();
        state.set_item_heights(vec![20, 20]);
        state.layout(
            Rect {
                x: 0,
                y: 0,
                width: 120,
                height: 120,
            },
            0,
        );

        let (_, interaction) = state.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: pointer(10, 25),
        });

        assert_eq!(interaction, ListInteraction::Selected(1));
        assert_eq!(state.selected_index, Some(1));
    }
}
