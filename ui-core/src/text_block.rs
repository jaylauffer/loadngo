use crate::{
    component::Component,
    geometry::{Insets, Rect},
    paint::{PaintOp, TextLayoutMode, TextOverflow, TextStyle, VerticalAlign},
};

#[derive(Debug, Clone, PartialEq)]
pub struct TextBlockModel {
    pub bounds: Rect,
    pub text: String,
    pub style: TextStyle,
    pub padding: Insets,
}

impl TextBlockModel {
    pub fn new(text: impl Into<String>, bounds: Rect) -> Self {
        let mut style = TextStyle::default();
        style.layout_mode = TextLayoutMode::MultiLine;
        style.vertical_align = VerticalAlign::Top;
        style.overflow = TextOverflow::Clip;
        Self {
            bounds,
            text: text.into(),
            style,
            padding: Insets::default(),
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
        let mut style = self.style.clone();
        style.layout_mode = TextLayoutMode::MultiLine;
        scene.push(PaintOp::Text {
            rect: self.content_rect(),
            clip_rect: None,
            text: self.text.clone(),
            style,
        });
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextBlock {
    pub id: i32,
    pub model: TextBlockModel,
}

impl TextBlock {
    pub fn new(id: i32, text: impl Into<String>, bounds: Rect) -> Self {
        Self {
            id,
            model: TextBlockModel::new(text, bounds),
        }
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        self.model.paint(scene);
    }
}

impl Component for TextBlock {
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
    use super::{TextBlock, TextBlockModel};
    use crate::{
        component::Component,
        geometry::{Color, Insets, Rect},
        paint::{PaintOp, TextLayoutMode, VerticalAlign},
    };

    #[test]
    fn text_block_model_paints_multiline_text_inside_padding() {
        let mut block = TextBlockModel::new(
            "Selection\nop #0001\nScene bg.png",
            Rect {
                x: 10.0,
                y: 20.0,
                width: 220.0,
                height: 120.0,
            },
        );
        block.padding = Insets {
            left: 8.0,
            top: 6.0,
            right: 12.0,
            bottom: 10.0,
        };
        block.style.color = Color::rgba(240, 240, 220, 255);

        let mut ops = Vec::new();
        block.paint(&mut ops);

        assert_eq!(
            ops,
            vec![PaintOp::Text {
                rect: Rect {
                    x: 18.0,
                    y: 26.0,
                    width: 200.0,
                    height: 104.0,
                },
                clip_rect: None,
                text: "Selection\nop #0001\nScene bg.png".to_string(),
                style: block.style.clone(),
            }]
        );
        let PaintOp::Text { style, .. } = &ops[0] else {
            panic!("expected text op");
        };
        assert_eq!(style.layout_mode, TextLayoutMode::MultiLine);
        assert_eq!(style.vertical_align, VerticalAlign::Top);
    }

    #[test]
    fn text_block_component_updates_bounds() {
        let mut block = TextBlock::new(
            29,
            "Status\nLoading",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 60.0,
            },
        );
        block.set_bounds(Rect {
            x: 5.0,
            y: 6.0,
            width: 140.0,
            height: 90.0,
        });
        assert_eq!(
            block.bounds(),
            Rect {
                x: 5.0,
                y: 6.0,
                width: 140.0,
                height: 90.0,
            }
        );
    }
}
