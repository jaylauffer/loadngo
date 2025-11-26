use crate::{
    component::Component,
    geometry::{Color, Insets, Rect},
    paint::PaintOp,
    single_line_text_box_height,
};

#[derive(Debug, Clone, PartialEq)]
pub struct ListRowModel {
    pub bounds: Rect,
    pub padding: Insets,
    pub background: Option<Color>,
    pub border: Option<Color>,
}

impl ListRowModel {
    pub fn new(bounds: Rect) -> Self {
        Self {
            bounds,
            padding: Insets {
                left: 10.0,
                top: 2.0,
                right: 10.0,
                bottom: 2.0,
            },
            background: None,
            border: None,
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

    pub fn leading_rect(&self, width: f32, _gap: f32) -> Rect {
        let content = self.content_rect();
        Rect {
            x: content.x,
            y: content.y,
            width: width.max(0.0).min(content.width),
            height: content.height,
        }
    }

    pub fn trailing_rect(&self, width: f32, _gap: f32) -> Rect {
        let content = self.content_rect();
        let width = width.max(0.0).min(content.width);
        Rect {
            x: content.x + content.width - width,
            y: content.y,
            width,
            height: content.height,
        }
    }

    pub fn body_rect(&self, leading_width: f32, trailing_width: f32, gap: f32) -> Rect {
        let content = self.content_rect();
        let leading_width = leading_width.max(0.0);
        let trailing_width = trailing_width.max(0.0);
        let gap = gap.max(0.0);
        let x = content.x + leading_width + if leading_width > 0.0 { gap } else { 0.0 };
        let right_reserved = trailing_width + if trailing_width > 0.0 { gap } else { 0.0 };
        let width = (content.width - (x - content.x) - right_reserved).max(0.0);
        Rect {
            x,
            y: content.y,
            width,
            height: content.height,
        }
    }

    pub fn single_line_body_rect(
        &self,
        font_size: u16,
        leading_width: f32,
        trailing_width: f32,
        gap: f32,
    ) -> Rect {
        let mut rect = self.body_rect(leading_width, trailing_width, gap);
        let line_box_height = single_line_text_box_height(font_size);
        rect.y += (rect.height - line_box_height).max(0.0) * 0.5;
        rect.height = line_box_height.max(rect.height);
        rect
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
pub struct ListRow {
    pub id: i32,
    pub model: ListRowModel,
}

impl ListRow {
    pub fn new(id: i32, bounds: Rect) -> Self {
        Self {
            id,
            model: ListRowModel::new(bounds),
        }
    }
}

impl Component for ListRow {
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
    use super::{ListRow, ListRowModel};
    use crate::{
        component::Component,
        geometry::{Color, Rect},
        paint::PaintOp,
        single_line_text_box_height,
    };

    #[test]
    fn list_row_exposes_content_and_body_slots() {
        let row = ListRowModel::new(Rect {
            x: 10.0,
            y: 20.0,
            width: 300.0,
            height: 36.0,
        });

        assert_eq!(
            row.content_rect(),
            Rect {
                x: 20.0,
                y: 22.0,
                width: 280.0,
                height: 32.0,
            }
        );
        assert_eq!(
            row.body_rect(24.0, 40.0, 8.0),
            Rect {
                x: 52.0,
                y: 22.0,
                width: 200.0,
                height: 32.0,
            }
        );
    }

    #[test]
    fn list_row_paints_background_and_border() {
        let mut row = ListRowModel::new(Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 30.0,
        });
        row.background = Some(Color::rgba(10, 20, 30, 255));
        row.border = Some(Color::rgba(200, 210, 220, 255));

        let mut ops = Vec::new();
        row.paint(&mut ops);

        assert_eq!(
            ops,
            vec![
                PaintOp::FillRect {
                    rect: row.bounds,
                    color: Color::rgba(10, 20, 30, 255),
                },
                PaintOp::StrokeRect {
                    rect: row.bounds,
                    color: Color::rgba(200, 210, 220, 255),
                },
            ]
        );
    }

    #[test]
    fn list_row_component_updates_bounds() {
        let mut row = ListRow::new(
            51,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 32.0,
            },
        );
        row.set_bounds(Rect {
            x: 5.0,
            y: 7.0,
            width: 180.0,
            height: 40.0,
        });
        assert_eq!(
            row.bounds(),
            Rect {
                x: 5.0,
                y: 7.0,
                width: 180.0,
                height: 40.0,
            }
        );
    }

    #[test]
    fn list_row_single_line_body_rect_uses_shared_box_height() {
        let row = ListRowModel::new(Rect {
            x: 10.0,
            y: 20.0,
            width: 300.0,
            height: 35.0,
        });

        let body = row.single_line_body_rect(17, 0.0, 0.0, 0.0);

        assert_eq!(body.x, 20.0);
        assert_eq!(
            body.y,
            22.0 + (31.0 - single_line_text_box_height(17)) * 0.5
        );
        assert_eq!(body.height, 31.0);
    }
}
