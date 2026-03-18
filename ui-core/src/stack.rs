use crate::{
    component::Component,
    geometry::{Color, Insets, Rect},
    paint::PaintOp,
};

#[derive(Debug, Clone, PartialEq)]
pub struct VerticalStackModel {
    pub bounds: Rect,
    pub padding: Insets,
    pub gap: f32,
    pub background: Option<Color>,
    pub border: Option<Color>,
    pub child_heights: Vec<f32>,
    pub child_bounds: Vec<Rect>,
}

impl VerticalStackModel {
    pub fn new(bounds: Rect) -> Self {
        Self {
            bounds,
            padding: Insets::default(),
            gap: 0.0,
            background: None,
            border: None,
            child_heights: Vec::new(),
            child_bounds: Vec::new(),
        }
    }

    pub fn set_bounds(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    pub fn set_child_heights(&mut self, child_heights: Vec<f32>) {
        self.child_heights = child_heights;
        self.layout();
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

    pub fn layout(&mut self) {
        self.child_bounds.clear();
        let content = self.content_rect();
        let mut y = content.y;
        for height in &self.child_heights {
            let rect = Rect {
                x: content.x,
                y,
                width: content.width,
                height: (*height).max(0.0),
            };
            self.child_bounds.push(rect);
            y += rect.height + self.gap.max(0.0);
        }
    }

    pub fn total_content_height(&self) -> f32 {
        let gaps = self.child_heights.len().saturating_sub(1) as f32 * self.gap.max(0.0);
        self.child_heights.iter().sum::<f32>() + gaps
    }

    pub fn child_rect(&self, index: usize) -> Option<Rect> {
        self.child_bounds.get(index).copied()
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
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
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerticalStack {
    pub id: i32,
    pub model: VerticalStackModel,
}

impl VerticalStack {
    pub fn new(id: i32, bounds: Rect) -> Self {
        Self {
            id,
            model: VerticalStackModel::new(bounds),
        }
    }
}

impl Component for VerticalStack {
    fn bounds(&self) -> Rect {
        self.model.bounds
    }

    fn set_bounds(&mut self, rect: Rect) {
        self.model.set_bounds(rect);
        self.model.layout();
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
    use super::{VerticalStack, VerticalStackModel};
    use crate::{
        component::Component,
        geometry::{Color, Insets, Rect},
        paint::PaintOp,
    };

    #[test]
    fn vertical_stack_lays_out_children_with_padding_and_gap() {
        let mut stack = VerticalStackModel::new(Rect {
            x: 10.0,
            y: 20.0,
            width: 200.0,
            height: 220.0,
        });
        stack.padding = Insets {
            left: 8.0,
            top: 10.0,
            right: 12.0,
            bottom: 14.0,
        };
        stack.gap = 6.0;
        stack.set_child_heights(vec![24.0, 30.0, 18.0]);

        assert_eq!(
            stack.child_bounds,
            vec![
                Rect {
                    x: 18.0,
                    y: 30.0,
                    width: 180.0,
                    height: 24.0,
                },
                Rect {
                    x: 18.0,
                    y: 60.0,
                    width: 180.0,
                    height: 30.0,
                },
                Rect {
                    x: 18.0,
                    y: 96.0,
                    width: 180.0,
                    height: 18.0,
                },
            ]
        );
        assert_eq!(stack.total_content_height(), 84.0);
    }

    #[test]
    fn vertical_stack_paints_background_and_border() {
        let mut stack = VerticalStackModel::new(Rect {
            x: 0.0,
            y: 0.0,
            width: 80.0,
            height: 60.0,
        });
        stack.background = Some(Color::rgba(10, 20, 30, 255));
        stack.border = Some(Color::rgba(200, 210, 220, 255));

        let mut ops = Vec::new();
        stack.paint(&mut ops);

        assert_eq!(
            ops,
            vec![
                PaintOp::FillRect {
                    rect: stack.bounds,
                    color: Color::rgba(10, 20, 30, 255),
                },
                PaintOp::StrokeRect {
                    rect: stack.bounds,
                    color: Color::rgba(200, 210, 220, 255),
                },
            ]
        );
    }

    #[test]
    fn vertical_stack_component_updates_bounds() {
        let mut stack = VerticalStack::new(
            31,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 120.0,
            },
        );
        stack.set_bounds(Rect {
            x: 4.0,
            y: 6.0,
            width: 140.0,
            height: 180.0,
        });
        assert_eq!(
            stack.bounds(),
            Rect {
                x: 4.0,
                y: 6.0,
                width: 140.0,
                height: 180.0,
            }
        );
    }
}
