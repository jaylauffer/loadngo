use crate::{
    component::Component,
    geometry::{Color, Insets, Rect},
    paint::PaintOp,
    scroll::ScrollRegionModel,
};

#[derive(Debug, Clone, PartialEq)]
pub struct ScrollContainerModel {
    pub bounds: Rect,
    pub padding: Insets,
    pub background: Option<Color>,
    pub border: Option<Color>,
    pub scroll_region: ScrollRegionModel,
}

impl ScrollContainerModel {
    pub fn new(bounds: Rect) -> Self {
        let scroll_region = ScrollRegionModel::new(bounds, 0.0);
        Self {
            bounds,
            padding: Insets::default(),
            background: None,
            border: None,
            scroll_region,
        }
    }

    pub fn set_bounds(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.scroll_region.set_viewport(self.content_rect());
    }

    pub fn content_rect(&self) -> Rect {
        let x = self.bounds.x + self.padding.left;
        let y = self.bounds.y + self.padding.top;
        let width = (self.bounds.width - self.padding.left - self.padding.right).max(0.0);
        let height = (self.bounds.height - self.padding.top - self.padding.bottom).max(0.0);
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    pub fn set_content_height(&mut self, content_height: f32) {
        self.scroll_region.set_viewport(self.content_rect());
        self.scroll_region.set_content_height(content_height);
    }

    pub fn apply_scroll_delta(&mut self, delta: f32) {
        self.scroll_region.apply_scroll_delta(delta);
    }

    pub fn content_origin_y(&self) -> f32 {
        self.scroll_region.content_origin_y(0.0)
    }

    pub fn visible(&self, y: f32, height: f32) -> bool {
        self.scroll_region.visible(y, height)
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>, indicator_color: Color) {
        if let Some(color) = self.background {
            scene.push(PaintOp::FillRect {
                rect: self.bounds,
                color,
            });
        }
        if let Some(color) = self.border {
            scene.push(PaintOp::StrokeRect {
                rect: self.bounds,
                color,
            });
        }
        self.scroll_region.paint_indicator(scene, indicator_color);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScrollContainer {
    pub id: i32,
    pub model: ScrollContainerModel,
}

impl ScrollContainer {
    pub fn new(id: i32, bounds: Rect) -> Self {
        Self {
            id,
            model: ScrollContainerModel::new(bounds),
        }
    }
}

impl Component for ScrollContainer {
    fn bounds(&self) -> Rect {
        self.model.bounds
    }

    fn set_bounds(&mut self, rect: Rect) {
        self.model.set_bounds(rect);
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
    use super::{ScrollContainer, ScrollContainerModel};
    use crate::{
        component::Component,
        geometry::{Color, Insets, Rect},
        paint::PaintOp,
    };

    #[test]
    fn scroll_container_uses_padded_viewport_for_scroll_region() {
        let mut container = ScrollContainerModel::new(Rect {
            x: 10.0,
            y: 20.0,
            width: 200.0,
            height: 180.0,
        });
        container.padding = Insets {
            left: 8.0,
            top: 10.0,
            right: 12.0,
            bottom: 14.0,
        };
        container.set_content_height(320.0);

        assert_eq!(
            container.scroll_region.viewport,
            Rect {
                x: 18.0,
                y: 30.0,
                width: 180.0,
                height: 156.0,
            }
        );
    }

    #[test]
    fn scroll_container_paints_chrome_and_indicator() {
        let mut container = ScrollContainerModel::new(Rect {
            x: 0.0,
            y: 0.0,
            width: 120.0,
            height: 100.0,
        });
        container.background = Some(Color::rgba(10, 20, 30, 255));
        container.border = Some(Color::rgba(200, 210, 220, 255));
        container.set_content_height(220.0);

        let mut ops = Vec::new();
        container.paint(&mut ops, Color::rgba(92, 141, 232, 230));

        assert!(matches!(ops[0], PaintOp::FillRect { .. }));
        assert!(matches!(ops[1], PaintOp::StrokeRect { .. }));
        assert!(ops.len() >= 3);
    }

    #[test]
    fn scroll_container_component_updates_bounds() {
        let mut container = ScrollContainer::new(
            41,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 120.0,
            },
        );
        container.set_bounds(Rect {
            x: 6.0,
            y: 8.0,
            width: 160.0,
            height: 220.0,
        });
        assert_eq!(
            container.bounds(),
            Rect {
                x: 6.0,
                y: 8.0,
                width: 160.0,
                height: 220.0,
            }
        );
    }
}
