use crate::{
    component::Component,
    geometry::{Color, Insets, Rect},
    paint::PaintOp,
};

#[derive(Debug, Clone, PartialEq)]
pub struct PanelModel {
    pub bounds: Rect,
    pub padding: Insets,
    pub background: Option<Color>,
    pub border: Option<Color>,
}

impl PanelModel {
    pub fn new(bounds: Rect) -> Self {
        Self {
            bounds,
            padding: Insets::default(),
            background: Some(Color::rgba(0xf0, 0xf0, 0xf0, 0xff)),
            border: Some(Color::rgba(0x86, 0x8d, 0xa0, 0xd4)),
        }
    }

    pub fn set_bounds(&mut self, bounds: Rect) {
        self.bounds = bounds;
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
pub struct Panel {
    pub id: i32,
    pub model: PanelModel,
}

impl Panel {
    pub fn new(id: i32, bounds: Rect) -> Self {
        Self {
            id,
            model: PanelModel::new(bounds),
        }
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        self.model.paint(scene);
    }
}

impl Component for Panel {
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
    use super::{Panel, PanelModel};
    use crate::{
        component::Component,
        geometry::{Color, Insets, Rect},
        paint::PaintOp,
    };

    #[test]
    fn panel_model_emits_fill_and_border() {
        let mut panel = PanelModel::new(Rect {
            x: 12.0,
            y: 24.0,
            width: 160.0,
            height: 90.0,
        });
        panel.background = Some(Color::rgba(10, 20, 30, 255));
        panel.border = Some(Color::rgba(200, 210, 220, 255));

        let mut ops = Vec::new();
        panel.paint(&mut ops);

        assert_eq!(
            ops,
            vec![
                PaintOp::FillRect {
                    rect: panel.bounds,
                    color: Color::rgba(10, 20, 30, 255),
                },
                PaintOp::StrokeRect {
                    rect: panel.bounds,
                    color: Color::rgba(200, 210, 220, 255),
                },
            ]
        );
    }

    #[test]
    fn panel_content_rect_uses_padding_in_logical_space() {
        let mut panel = PanelModel::new(Rect {
            x: 4.0,
            y: 6.0,
            width: 100.0,
            height: 80.0,
        });
        panel.padding = Insets {
            left: 8.0,
            top: 10.0,
            right: 12.0,
            bottom: 14.0,
        };

        assert_eq!(
            panel.content_rect(),
            Rect {
                x: 12.0,
                y: 16.0,
                width: 80.0,
                height: 56.0,
            }
        );
    }

    #[test]
    fn panel_component_updates_bounds() {
        let mut panel = Panel::new(
            9,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 40.0,
                height: 50.0,
            },
        );
        panel.set_bounds(Rect {
            x: 3.0,
            y: 7.0,
            width: 60.0,
            height: 70.0,
        });

        assert_eq!(
            panel.bounds(),
            Rect {
                x: 3.0,
                y: 7.0,
                width: 60.0,
                height: 70.0,
            }
        );
    }
}
