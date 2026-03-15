use crate::{
    component::Component,
    geometry::{Color, Rect},
    input::{Key, UiEvent},
    paint::{PaintOp, TextStyle},
    widget::WidgetResponse,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabPage {
    pub title: String,
    pub content_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabbedContainer {
    pub bounds: Rect,
    pub pages: Vec<TabPage>,
    pub selected: usize,
}

impl TabbedContainer {
    pub fn new(bounds: Rect) -> Self {
        Self {
            bounds,
            pages: Vec::new(),
            selected: 0,
        }
    }

    pub fn add_page(&mut self, title: impl Into<String>, content_id: Option<u64>) {
        self.pages.push(TabPage {
            title: title.into(),
            content_id,
        });
    }

    pub fn select(&mut self, index: usize) -> bool {
        if index < self.pages.len() {
            self.selected = index;
            true
        } else {
            false
        }
    }

    pub fn selected_page(&self) -> Option<&TabPage> {
        self.pages.get(self.selected)
    }

    pub fn tab_rects(&self) -> Vec<Rect> {
        if self.pages.is_empty() {
            return Vec::new();
        }
        let width = (self.bounds.width / self.pages.len() as i32).max(1);
        self.pages
            .iter()
            .enumerate()
            .map(|(index, _)| Rect {
                x: self.bounds.x + (index as i32 * width),
                y: self.bounds.y,
                width,
                height: 32,
            })
            .collect()
    }

    pub fn handle_event(&mut self, event: UiEvent) -> WidgetResponse {
        match event {
            UiEvent::PointerReleased { state, .. } => {
                for (index, rect) in self.tab_rects().iter().enumerate() {
                    if rect.contains(state.position) && self.select(index) {
                        return WidgetResponse::redraw();
                    }
                }
            }
            UiEvent::KeyPressed {
                key: Key::Right, ..
            } => {
                if self.pages.is_empty() {
                    return WidgetResponse::default();
                }
                let next = (self.selected + 1).min(self.pages.len() - 1);
                if self.select(next) {
                    return WidgetResponse::redraw();
                }
            }
            UiEvent::KeyPressed { key: Key::Left, .. } => {
                if self.select(self.selected.saturating_sub(1)) {
                    return WidgetResponse::redraw();
                }
            }
            _ => {}
        }
        WidgetResponse::default()
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        scene.push(PaintOp::FillRect {
            rect: self.bounds,
            color: Color::rgba(0xe5, 0xe7, 0xe0, 0xff),
        });
        for (index, rect) in self.tab_rects().into_iter().enumerate() {
            let selected = index == self.selected;
            scene.push(PaintOp::FillRect {
                rect,
                color: if selected {
                    Color::rgba(0xf6, 0xd6, 0x99, 0xff)
                } else {
                    Color::rgba(0xd9, 0xdf, 0xd2, 0xff)
                },
            });
            scene.push(PaintOp::StrokeRect {
                rect,
                color: Color::rgba(0x6d, 0x7d, 0x6e, 0xff),
            });
            if let Some(page) = self.pages.get(index) {
                scene.push(PaintOp::Text {
                    rect,
                    text: page.title.clone(),
                    style: TextStyle {
                        centered: true,
                        ..TextStyle::default()
                    },
                });
            }
        }
    }
}

impl Component for TabbedContainer {
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
    use super::TabbedContainer;
    use crate::{Modifiers, PointerButton, PointerState, Rect, UiEvent};

    #[test]
    fn tab_selection_tracks_selected_page() {
        let mut tabs = TabbedContainer::new(Rect {
            x: 0,
            y: 0,
            width: 200,
            height: 200,
        });
        tabs.add_page("one", Some(1));
        tabs.add_page("two", Some(2));

        assert!(tabs.select(1));
        assert_eq!(
            tabs.selected_page().map(|page| page.title.as_str()),
            Some("two")
        );
        assert!(!tabs.select(2));
    }

    #[test]
    fn tab_pointer_release_selects_clicked_tab() {
        let mut tabs = TabbedContainer::new(Rect {
            x: 0,
            y: 0,
            width: 200,
            height: 200,
        });
        tabs.add_page("one", Some(1));
        tabs.add_page("two", Some(2));

        let response = tabs.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: PointerState::mouse(crate::Point { x: 150, y: 10 }, Modifiers::default()),
        });

        assert!(response.request_redraw);
        assert_eq!(tabs.selected, 1);
    }
}
