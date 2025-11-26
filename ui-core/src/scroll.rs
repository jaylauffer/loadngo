use crate::{
    geometry::{Color, Point, Rect},
    paint::PaintOp,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollbarAxis {
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollbarDragState {
    pub pointer_offset: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScrollbarModel {
    pub axis: ScrollbarAxis,
    pub track_rect: Rect,
    pub offset: f32,
    pub viewport_span: f32,
    pub content_span: f32,
    content_span_known: bool,
    pub min_thumb_span: f32,
}

impl ScrollbarModel {
    pub fn new(axis: ScrollbarAxis, track_rect: Rect, offset: f32) -> Self {
        Self {
            axis,
            track_rect,
            offset: offset.max(0.0),
            viewport_span: match axis {
                ScrollbarAxis::Vertical => track_rect.height,
                ScrollbarAxis::Horizontal => track_rect.width,
            },
            content_span: match axis {
                ScrollbarAxis::Vertical => track_rect.height,
                ScrollbarAxis::Horizontal => track_rect.width,
            },
            content_span_known: false,
            min_thumb_span: match axis {
                ScrollbarAxis::Vertical => 24.0,
                ScrollbarAxis::Horizontal => 32.0,
            },
        }
    }

    pub fn set_track_rect(&mut self, track_rect: Rect) {
        self.track_rect = track_rect;
        self.clamp_offset();
    }

    pub fn set_viewport_span(&mut self, viewport_span: f32) {
        self.viewport_span = viewport_span.max(0.0);
        self.clamp_offset();
    }

    pub fn set_content_span(&mut self, content_span: f32) {
        self.content_span = content_span.max(self.viewport_span);
        self.content_span_known = true;
        self.clamp_offset();
    }

    pub fn apply_scroll_delta(&mut self, delta: f32) {
        self.offset += delta;
        if self.content_span_known {
            self.clamp_offset();
        } else {
            self.offset = self.offset.max(0.0);
        }
    }

    pub fn max_offset(&self) -> f32 {
        (self.content_span - self.viewport_span).max(0.0)
    }

    pub fn is_scrollable(&self) -> bool {
        self.content_span_known
            && self.max_offset() > 0.0
            && self.primary_span(self.track_rect) > 0.0
            && self.cross_span(self.track_rect) > 0.0
    }

    pub fn indicator_thumb_rect(&self) -> Option<Rect> {
        if !self.is_scrollable() {
            return None;
        }
        let max_offset = self.max_offset();
        let thumb_span = self.thumb_span(self.primary_span(self.track_rect), max_offset);
        let t = if max_offset > 0.0 {
            (self.offset / max_offset).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let thumb_start = self.primary_start(self.track_rect)
            + (self.primary_span(self.track_rect) - thumb_span) * t;
        Some(self.rect_with_primary_range(self.track_rect, thumb_start, thumb_span))
    }

    pub fn interactive_rect(&self, pad_cross: f32, pad_main: f32) -> Option<Rect> {
        if !self.is_scrollable() {
            return None;
        }
        Some(match self.axis {
            ScrollbarAxis::Vertical => Rect {
                x: self.track_rect.x - pad_cross,
                y: self.track_rect.y - pad_main,
                width: self.track_rect.width + pad_cross * 2.0,
                height: self.track_rect.height + pad_main * 2.0,
            },
            ScrollbarAxis::Horizontal => Rect {
                x: self.track_rect.x - pad_main,
                y: self.track_rect.y - pad_cross,
                width: self.track_rect.width + pad_main * 2.0,
                height: self.track_rect.height + pad_cross * 2.0,
            },
        })
    }

    pub fn begin_indicator_drag(&self, pointer: Point) -> Option<ScrollbarDragState> {
        let thumb = self.indicator_thumb_rect()?;
        if !thumb.contains(pointer) {
            return None;
        }
        Some(ScrollbarDragState {
            pointer_offset: self.primary_point(pointer) - self.primary_start(thumb),
        })
    }

    pub fn drag_indicator_to(&mut self, pointer: Point, drag_state: ScrollbarDragState) {
        if !self.is_scrollable() {
            return;
        }
        let thumb = self
            .indicator_thumb_rect()
            .unwrap_or(self.rect_with_primary_range(
                self.track_rect,
                self.primary_start(self.track_rect),
                0.0,
            ));
        self.set_offset_from_thumb_start(
            self.primary_point(pointer)
                - self.primary_start(self.track_rect)
                - drag_state.pointer_offset,
            self.primary_span(thumb),
        );
    }

    pub fn scroll_to_indicator_position(&mut self, pointer: Point) {
        if !self.is_scrollable() {
            return;
        }
        let thumb_span = self.thumb_span(self.primary_span(self.track_rect), self.max_offset());
        self.set_offset_from_thumb_start(
            self.primary_point(pointer) - self.primary_start(self.track_rect) - thumb_span * 0.5,
            thumb_span,
        );
    }

    pub fn paint_indicator(
        &self,
        scene: &mut Vec<PaintOp>,
        track_color: Color,
        thumb_color: Color,
    ) {
        if !self.is_scrollable() {
            return;
        }
        scene.push(PaintOp::FillRect {
            rect: self.track_rect,
            color: track_color,
        });
        if let Some(thumb) = self.indicator_thumb_rect() {
            scene.push(PaintOp::FillRect {
                rect: thumb,
                color: thumb_color,
            });
        }
    }

    fn clamp_offset(&mut self) {
        self.offset = self.offset.clamp(0.0, self.max_offset());
    }

    fn thumb_span(&self, track_span: f32, max_offset: f32) -> f32 {
        let ratio = (track_span / (track_span + max_offset)).clamp(0.12, 1.0);
        (track_span * ratio).clamp(self.min_thumb_span, track_span)
    }

    fn set_offset_from_thumb_start(&mut self, thumb_start: f32, thumb_span: f32) {
        let max_offset = self.max_offset();
        if max_offset <= 0.0 {
            self.offset = 0.0;
            return;
        }
        let travel = (self.primary_span(self.track_rect) - thumb_span).max(0.0);
        let local = thumb_start.clamp(0.0, travel);
        let t = if travel > 0.0 { local / travel } else { 0.0 };
        self.offset = max_offset * t;
        self.clamp_offset();
    }

    fn primary_start(&self, rect: Rect) -> f32 {
        match self.axis {
            ScrollbarAxis::Vertical => rect.y,
            ScrollbarAxis::Horizontal => rect.x,
        }
    }

    fn primary_span(&self, rect: Rect) -> f32 {
        match self.axis {
            ScrollbarAxis::Vertical => rect.height,
            ScrollbarAxis::Horizontal => rect.width,
        }
    }

    fn cross_span(&self, rect: Rect) -> f32 {
        match self.axis {
            ScrollbarAxis::Vertical => rect.width,
            ScrollbarAxis::Horizontal => rect.height,
        }
    }

    fn primary_point(&self, point: Point) -> f32 {
        match self.axis {
            ScrollbarAxis::Vertical => point.y,
            ScrollbarAxis::Horizontal => point.x,
        }
    }

    fn rect_with_primary_range(&self, track: Rect, start: f32, span: f32) -> Rect {
        match self.axis {
            ScrollbarAxis::Vertical => Rect {
                x: track.x,
                y: start,
                width: track.width,
                height: span,
            },
            ScrollbarAxis::Horizontal => Rect {
                x: start,
                y: track.y,
                width: span,
                height: track.height,
            },
        }
    }
}

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
        let max_thumb = track_height.max(0.0);
        if max_thumb <= 0.0 {
            return 0.0;
        }
        let min_thumb = Self::MIN_THUMB_HEIGHT.min(max_thumb);
        let ratio = (track_height / (track_height + max_offset)).clamp(0.12, 1.0);
        (track_height * ratio).clamp(min_thumb, max_thumb)
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
    fn scroll_region_tiny_track_never_panics_when_painting_indicator() {
        let mut region = ScrollRegionModel::new(
            Rect {
                x: 10.0,
                y: 20.0,
                width: 180.0,
                height: 0.0,
            },
            10.0,
        );
        region.set_content_height(300.0);
        let mut ops = Vec::new();
        region.paint_indicator(&mut ops, Color::rgba(92, 141, 232, 230));
        assert!(!ops.is_empty());
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
