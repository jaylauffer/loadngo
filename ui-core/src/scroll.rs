use crate::{
    geometry::{Color, Rect},
    paint::PaintOp,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollThumbDragState {
    pub pointer_offset_y: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScrollRegionModel {
    pub viewport: Rect,
    pub offset: f32,
    pub content_height: f32,
    content_height_known: bool,
}

impl ScrollRegionModel {
    const INDICATOR_WIDTH: f32 = 6.0;
    const INDICATOR_RIGHT_INSET: f32 = 8.0;
    const MIN_THUMB_HEIGHT: f32 = 24.0;

    pub fn new(viewport: Rect, offset: f32) -> Self {
        Self {
            viewport,
            offset: offset.max(0.0),
            content_height: viewport.height,
            content_height_known: false,
        }
    }

    pub fn set_viewport(&mut self, viewport: Rect) {
        self.viewport = viewport;
        self.clamp_offset();
    }

    pub fn set_content_height(&mut self, content_height: f32) {
        self.content_height = content_height.max(self.viewport.height);
        self.content_height_known = true;
        self.clamp_offset();
    }

    pub fn apply_scroll_delta(&mut self, delta: f32) {
        self.offset += delta;
        if self.content_height_known {
            self.clamp_offset();
        } else {
            self.offset = self.offset.max(0.0);
        }
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

    pub fn indicator_track_rect(&self) -> Option<Rect> {
        if !self.is_scrollable() {
            return None;
        }
        Some(Rect {
            x: self.viewport.x + self.viewport.width - Self::INDICATOR_RIGHT_INSET,
            y: self.viewport.y,
            width: Self::INDICATOR_WIDTH,
            height: self.viewport.height,
        })
    }

    pub fn indicator_thumb_rect(&self) -> Option<Rect> {
        let track = self.indicator_track_rect()?;
        let max_offset = self.max_offset();
        let thumb_h = self.thumb_height(track.height, max_offset);
        let t = if max_offset > 0.0 {
            (self.offset / max_offset).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let thumb_y = track.y + (track.height - thumb_h) * t;
        Some(Rect {
            x: track.x,
            y: thumb_y,
            width: track.width,
            height: thumb_h,
        })
    }

    pub fn scroll_to_indicator_position(&mut self, pointer_y: f32) {
        let Some(track) = self.indicator_track_rect() else {
            return;
        };
        let thumb_h = self.thumb_height(track.height, self.max_offset());
        self.set_offset_from_thumb_top(pointer_y - track.y - thumb_h * 0.5);
    }

    pub fn begin_indicator_drag(&self, pointer_y: f32) -> Option<ScrollThumbDragState> {
        let thumb = self.indicator_thumb_rect()?;
        if pointer_y < thumb.y || pointer_y > thumb.y + thumb.height {
            return None;
        }
        Some(ScrollThumbDragState {
            pointer_offset_y: pointer_y - thumb.y,
        })
    }

    pub fn drag_indicator_to(&mut self, pointer_y: f32, drag_state: ScrollThumbDragState) {
        let Some(track) = self.indicator_track_rect() else {
            return;
        };
        self.set_offset_from_thumb_top(pointer_y - track.y - drag_state.pointer_offset_y);
    }

    pub fn paint_indicator(&self, scene: &mut Vec<PaintOp>, color: Color) {
        let Some(track) = self.indicator_track_rect() else {
            return;
        };
        scene.push(PaintOp::FillRect {
            rect: track,
            color: Color::rgba(30, 36, 52, 220),
        });
        if let Some(thumb) = self.indicator_thumb_rect() {
            scene.push(PaintOp::FillRect { rect: thumb, color });
        }
    }

    fn clamp_offset(&mut self) {
        self.offset = self.offset.clamp(0.0, self.max_offset());
    }

    fn is_scrollable(&self) -> bool {
        self.content_height_known && self.max_offset() > 0.0
    }

    fn thumb_height(&self, track_height: f32, max_offset: f32) -> f32 {
        let ratio = (track_height / (track_height + max_offset)).clamp(0.12, 1.0);
        (track_height * ratio).clamp(Self::MIN_THUMB_HEIGHT, track_height)
    }

    fn set_offset_from_thumb_top(&mut self, thumb_top: f32) {
        let Some(track) = self.indicator_track_rect() else {
            return;
        };
        let max_offset = self.max_offset();
        if max_offset <= 0.0 {
            self.offset = 0.0;
            return;
        }
        let thumb_h = self.thumb_height(track.height, max_offset);
        let travel = (track.height - thumb_h).max(0.0);
        let local_y = thumb_top.clamp(0.0, travel);
        let t = if travel > 0.0 { local_y / travel } else { 0.0 };
        self.offset = max_offset * t;
        self.clamp_offset();
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        geometry::{Color, Rect},
        paint::PaintOp,
    };

    use super::{ScrollRegionModel, ScrollThumbDragState};

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

    #[test]
    fn scroll_region_preserves_pre_layout_delta_until_content_height_is_known() {
        let mut region = ScrollRegionModel::new(
            Rect {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 200.0,
            },
            0.0,
        );
        region.apply_scroll_delta(48.0);
        assert_eq!(region.offset, 48.0);
        region.set_content_height(350.0);
        assert_eq!(region.offset, 48.0);
    }

    #[test]
    fn scroll_region_indicator_position_updates_offset() {
        let mut region = ScrollRegionModel::new(
            Rect {
                x: 10.0,
                y: 20.0,
                width: 180.0,
                height: 120.0,
            },
            0.0,
        );
        region.set_content_height(360.0);
        let track = region.indicator_track_rect().unwrap();
        region.scroll_to_indicator_position(track.y + track.height);
        assert!(region.offset > 0.0);
        assert_eq!(region.offset, region.max_offset());
        let thumb = region.indicator_thumb_rect().unwrap();
        assert!(thumb.y >= track.y);
        assert!(thumb.bottom() <= track.bottom());
    }

    #[test]
    fn scroll_region_thumb_drag_tracks_pointer_delta() {
        let mut region = ScrollRegionModel::new(
            Rect {
                x: 10.0,
                y: 20.0,
                width: 180.0,
                height: 120.0,
            },
            0.0,
        );
        region.set_content_height(360.0);
        let thumb = region.indicator_thumb_rect().unwrap();
        let drag = region
            .begin_indicator_drag(thumb.y + 8.0)
            .expect("thumb press should begin drag");
        assert_eq!(
            drag,
            ScrollThumbDragState {
                pointer_offset_y: 8.0
            }
        );
        region.drag_indicator_to(thumb.y + 48.0, drag);
        assert!(region.offset > 0.0);
    }

    #[test]
    fn scroll_region_thumb_drag_requires_press_inside_thumb() {
        let mut region = ScrollRegionModel::new(
            Rect {
                x: 10.0,
                y: 20.0,
                width: 180.0,
                height: 120.0,
            },
            0.0,
        );
        region.set_content_height(360.0);
        let thumb = region.indicator_thumb_rect().unwrap();
        assert!(region.begin_indicator_drag(thumb.y - 1.0).is_none());
        assert!(region.begin_indicator_drag(thumb.bottom() + 1.0).is_none());
    }
}
