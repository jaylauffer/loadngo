use crate::{
    component::Component,
    geometry::{Insets, Rect},
    paint::{PaintOp, TextLayoutMode, TextStyle, VerticalAlign},
    single_line_text_box_height,
};

#[derive(Debug, Clone, PartialEq)]
pub struct LabelModel {
    pub bounds: Rect,
    pub text: String,
    pub style: TextStyle,
    pub padding: Insets,
}

impl LabelModel {
    pub fn new(text: impl Into<String>, bounds: Rect) -> Self {
        Self {
            bounds,
            text: text.into(),
            style: TextStyle::default(),
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

    pub fn text_rect(&self) -> Rect {
        let mut rect = self.content_rect();
        if matches!(self.style.layout_mode, TextLayoutMode::SingleLine) {
            let line_box_height = single_line_text_box_height(self.style.font_size);
            let available_height = rect.height.max(0.0);
            let offset_y = match self.style.vertical_align {
                VerticalAlign::Top => 0.0,
                VerticalAlign::Middle => (available_height - line_box_height).max(0.0) * 0.5,
                VerticalAlign::Bottom => (available_height - line_box_height).max(0.0),
            };
            rect.y += offset_y;
            rect.height = line_box_height.max(available_height);
        }
        rect
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        scene.push(PaintOp::Text {
            rect: self.text_rect(),
            clip_rect: None,
            text: self.text.clone(),
            style: self.style.clone(),
        });
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    pub id: i32,
    pub model: LabelModel,
}

impl Label {
    pub fn new(id: i32, text: impl Into<String>, bounds: Rect) -> Self {
        Self {
            id,
            model: LabelModel::new(text, bounds),
        }
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        self.model.paint(scene);
    }
}

impl Component for Label {
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
    use super::{Label, LabelModel};
    use crate::{
        component::Component,
        geometry::{Color, Insets, Rect},
        paint::{PaintOp, TextLayoutMode, VerticalAlign},
    };

    #[test]
    fn label_model_paints_text_inside_padding() {
        let mut label = LabelModel::new(
            "Inspector",
            Rect {
                x: 10.0,
                y: 20.0,
                width: 200.0,
                height: 40.0,
            },
        );
        label.padding = Insets {
            left: 8.0,
            top: 6.0,
            right: 12.0,
            bottom: 4.0,
        };
        label.style.color = Color::rgba(255, 240, 220, 255);

        let mut ops = Vec::new();
        label.paint(&mut ops);

        assert_eq!(
            ops,
            vec![PaintOp::Text {
                rect: Rect {
                    x: 18.0,
                    y: 26.0,
                    width: 180.0,
                    height: 30.0,
                },
                clip_rect: None,
                text: "Inspector".to_string(),
                style: label.style.clone(),
            }]
        );
    }

    #[test]
    fn single_line_label_uses_shared_text_box_height() {
        let mut label = LabelModel::new(
            "Labels",
            Rect {
                x: 10.0,
                y: 20.0,
                width: 180.0,
                height: 24.0,
            },
        );
        label.style.font_size = 17;
        label.style.layout_mode = TextLayoutMode::SingleLine;
        label.style.vertical_align = VerticalAlign::Top;

        let text_rect = label.text_rect();

        assert_eq!(text_rect.y, 20.0);
        assert_eq!(text_rect.height, 24.0);
    }

    #[test]
    fn label_component_updates_bounds() {
        let mut label = Label::new(
            17,
            "Status",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 80.0,
                height: 20.0,
            },
        );
        label.set_bounds(Rect {
            x: 5.0,
            y: 6.0,
            width: 120.0,
            height: 24.0,
        });
        assert_eq!(
            label.bounds(),
            Rect {
                x: 5.0,
                y: 6.0,
                width: 120.0,
                height: 24.0,
            }
        );
    }
}
