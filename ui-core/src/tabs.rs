use crate::{
    component::Component,
    geometry::{Color, Rect},
    input::{Key, UiEvent},
    paint::{HorizontalAlign, PaintOp, TextLayoutMode, TextStyle, VerticalAlign},
    widget::WidgetResponse,
};

#[derive(Debug, Clone, PartialEq)]
pub struct TabPage {
    pub title: String,
    pub content_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TabbedContainer {
    pub bounds: Rect,
    pub pages: Vec<TabPage>,
    pub selected: usize,
    pub tab_strip_height: f32,
    pub background: Color,
    pub border_color: Color,
    pub selected_fill: Color,
    pub unselected_fill: Color,
    pub selected_text_color: Color,
    pub unselected_text_color: Color,
}

impl TabbedContainer {
    pub fn new(bounds: Rect) -> Self {
        Self {
            bounds,
            pages: Vec::new(),
            selected: 0,
            tab_strip_height: 32.0,
            background: Color::rgba(15, 19, 26, 255),
            border_color: Color::rgba(74, 84, 98, 255),
            selected_fill: Color::rgba(42, 77, 109, 255),
            unselected_fill: Color::rgba(24, 29, 38, 255),
            selected_text_color: Color::rgba(236, 239, 244, 255),
            unselected_text_color: Color::rgba(198, 206, 219, 255),
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

    pub fn selected_content_id(&self) -> Option<u64> {
        self.selected_page().and_then(|page| page.content_id)
    }

    pub fn content_rect(&self) -> Rect {
        Rect {
            x: self.bounds.x,
            y: self.bounds.y + self.tab_strip_height,
            width: self.bounds.width,
            height: (self.bounds.height - self.tab_strip_height).max(0.0),
        }
    }

    pub fn tab_rects(&self) -> Vec<Rect> {
        if self.pages.is_empty() {
            return Vec::new();
        }
        let width = (self.bounds.width / self.pages.len() as f32).max(1.0);
        self.pages
            .iter()
            .enumerate()
            .map(|(index, _)| Rect {
                x: self.bounds.x + (index as f32 * width),
                y: self.bounds.y,
                width,
                height: self.tab_strip_height,
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
            color: self.background,
        });
        scene.push(PaintOp::StrokeRect {
            rect: self.bounds,
            color: self.border_color,
        });
        for (index, rect) in self.tab_rects().into_iter().enumerate() {
            let selected = index == self.selected;
            scene.push(PaintOp::FillRect {
                rect,
                color: if selected {
                    self.selected_fill
                } else {
                    self.unselected_fill
                },
            });
            scene.push(PaintOp::StrokeRect {
                rect,
                color: self.border_color,
            });
            if let Some(page) = self.pages.get(index) {
                scene.push(PaintOp::Text {
                    rect,
                    clip_rect: None,
                    text: page.title.clone(),
                    style: TextStyle {
                        color: if selected {
                            self.selected_text_color
                        } else {
                            self.unselected_text_color
                        },
                        horizontal_align: HorizontalAlign::Center,
                        vertical_align: VerticalAlign::Middle,
                        layout_mode: TextLayoutMode::SingleLine,
                        ..TextStyle::default()
                    },
                });
            }
        }
    }
}

pub type TabGroupModel = TabbedContainer;

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
    use crate::{Color, Modifiers, PointerButton, PointerState, Rect, UiEvent};

    #[test]
    fn tab_selection_tracks_selected_page() {
        let mut tabs = TabbedContainer::new(Rect {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 200.0,
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
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 200.0,
        });
        tabs.add_page("one", Some(1));
        tabs.add_page("two", Some(2));

        let response = tabs.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: PointerState::mouse(crate::Point { x: 150.0, y: 10.0 }, Modifiers::default()),
        });

        assert!(response.request_redraw);
        assert_eq!(tabs.selected, 1);
    }

    #[test]
    fn tab_rects_split_width_in_logical_space() {
        let mut tabs = TabbedContainer::new(Rect {
            x: 5.5,
            y: 7.0,
            width: 101.0,
            height: 48.0,
        });
        tabs.add_page("one", Some(1));
        tabs.add_page("two", Some(2));
        tabs.add_page("three", Some(3));

        let rects = tabs.tab_rects();
        assert_eq!(rects.len(), 3);
        assert!((rects[0].x - 5.5).abs() < f32::EPSILON);
        assert!((rects[1].x - (5.5 + 101.0 / 3.0)).abs() < 0.001);
        assert!((rects[2].x - (5.5 + 2.0 * 101.0 / 3.0)).abs() < 0.001);
        assert!((rects[0].width - 101.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn content_rect_starts_below_tab_strip() {
        let tabs = TabbedContainer::new(Rect {
            x: 10.0,
            y: 20.0,
            width: 240.0,
            height: 180.0,
        });
        let content = tabs.content_rect();

        assert_eq!(content.x, 10.0);
        assert_eq!(content.y, 52.0);
        assert_eq!(content.width, 240.0);
        assert_eq!(content.height, 148.0);
    }

    #[test]
    fn default_tab_theme_matches_dark_panel_ui() {
        let tabs = TabbedContainer::new(Rect {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 120.0,
        });

        assert_eq!(tabs.background, Color::rgba(15, 19, 26, 255));
        assert_eq!(tabs.selected_fill, Color::rgba(42, 77, 109, 255));
        assert_eq!(tabs.unselected_text_color, Color::rgba(198, 206, 219, 255));
    }
}
