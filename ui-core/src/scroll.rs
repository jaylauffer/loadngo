use crate::{
    geometry::{Color, Rect},
    paint::PaintOp,
};

#[derive(Debug, Clone, PartialEq)]
pub struct ScrollRegionModel {
    pub viewport: Rect,
    pub offset: f32,
    pub content_height: f32,
}

impl ScrollRegionModel {
    pub fn new(viewport: Rect, offset: f32) -> Self {
        Self {
            viewport,
            offset: offset.max(0.0),
            content_height: viewport.height,
        }
    }

    pub fn set_viewport(&mut self, viewport: Rect) {
        self.viewport = viewport;
        self.clamp_offset();
    }

    pub fn set_content_height(&mut self, content_height: f32) {
        self.content_height = content_height.max(self.viewport.height);
        self.clamp_offset();
    }

    pub fn apply_scroll_delta(&mut self, delta: f32) {
        self.offset += delta;
        self.clamp_offset();
    }

    pub fn max_offset(&self) -> f32 {
        (self.content_height - self.viewport.height).max(0.0)
    }

    pub fn content_origin_y(&self, padding_top: f32) -> f32 {
        self.viewport.y + padding_top - self.offset
    }

    pub fn visible(&self, y: f32, height: f32) -> bool {
        let view_top = self.viewport.y;
        let view_bottom = self.viewport.y + self.viewport.height;
        y + height >= view_top && y <= view_bottom
    }

    pub fn paint_indicator(&self, scene: &mut Vec<PaintOp>, color: Color) {
        let max_offset = self.max_offset();
        if max_offset <= 0.0 {
            return;
        }
        let x = self.viewport.x + self.viewport.width - 8.0;
        let y = self.viewport.y;
        let height = self.viewport.height;
        scene.push(PaintOp::FillRect {
            rect: Rect {
                x,
                y,
                width: 6.0,
                height,
            },
            color: Color::rgba(30, 36, 52, 220),
        });
        let ratio = (height / (height + max_offset)).clamp(0.12, 1.0);
        let thumb_h = (height * ratio).clamp(24.0, height);
        let t = (self.offset / max_offset).clamp(0.0, 1.0);
        let thumb_y = y + (height - thumb_h) * t;
        scene.push(PaintOp::FillRect {
            rect: Rect {
                x,
                y: thumb_y,
                width: 6.0,
                height: thumb_h,
            },
            color,
        });
    }

    fn clamp_offset(&mut self) {
        self.offset = self.offset.clamp(0.0, self.max_offset());
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        geometry::{Color, Rect},
        paint::PaintOp,
    };

    use super::ScrollRegionModel;

    #[test]
    fn scroll_region_clamps_offset_to_content_bounds() {
        let mut region = ScrollRegionModel::new(
            Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 200.0,
            },
            0.0,
        );
        region.set_content_height(350.0);
        region.apply_scroll_delta(500.0);
        assert_eq!(region.offset, 150.0);
        region.apply_scroll_delta(-1000.0);
        assert_eq!(region.offset, 0.0);
    }

    #[test]
    fn scroll_region_visibility_uses_viewport_bounds() {
        let region = ScrollRegionModel::new(
            Rect {
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 120.0,
            },
            0.0,
        );
        assert!(region.visible(20.0, 24.0));
        assert!(region.visible(130.0, 24.0));
        assert!(!region.visible(141.0, 10.0));
    }

    #[test]
    fn scroll_region_paints_indicator_only_when_scrollable() {
        let mut region = ScrollRegionModel::new(
            Rect {
                x: 10.0,
                y: 20.0,
                width: 180.0,
                height: 120.0,
            },
            10.0,
        );
        region.set_content_height(300.0);
        let mut ops = Vec::new();
        region.paint_indicator(&mut ops, Color::rgba(92, 141, 232, 230));
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], PaintOp::FillRect { .. }));
        assert!(matches!(ops[1], PaintOp::FillRect { .. }));
    }
}
