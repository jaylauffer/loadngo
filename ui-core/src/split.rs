use crate::{
    component::Component,
    geometry::{Color, Point, Rect},
    input::{PointerButton, UiEvent},
    paint::PaintOp,
    widget::WidgetResponse,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SplitDragState {
    pub pointer_id: u64,
    pub handle_offset: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SplitNodeModel {
    pub bounds: Rect,
    pub axis: SplitAxis,
    pub split_ratio: f32,
    pub min_first: f32,
    pub min_second: f32,
    pub handle_size: f32,
    pub hit_size: f32,
    pub hover: bool,
    pub dragging: bool,
    pub drag_state: Option<SplitDragState>,
    pub background: Option<Color>,
    pub border: Option<Color>,
}

impl SplitNodeModel {
    pub fn new(axis: SplitAxis, bounds: Rect) -> Self {
        Self {
            bounds,
            axis,
            split_ratio: 0.5,
            min_first: 0.0,
            min_second: 0.0,
            handle_size: 8.0,
            hit_size: 14.0,
            hover: false,
            dragging: false,
            drag_state: None,
            background: None,
            border: None,
        }
    }

    pub fn set_bounds(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.clamp_ratio();
    }

    pub fn set_split_ratio(&mut self, split_ratio: f32) {
        self.split_ratio = split_ratio;
        self.clamp_ratio();
    }

    pub fn first_rect(&self) -> Rect {
        match self.axis {
            SplitAxis::Horizontal => Rect {
                x: self.bounds.x,
                y: self.bounds.y,
                width: self.first_extent(),
                height: self.bounds.height,
            },
            SplitAxis::Vertical => Rect {
                x: self.bounds.x,
                y: self.bounds.y,
                width: self.bounds.width,
                height: self.first_extent(),
            },
        }
    }

    pub fn handle_rect(&self) -> Rect {
        match self.axis {
            SplitAxis::Horizontal => Rect {
                x: self.bounds.x + self.first_extent(),
                y: self.bounds.y,
                width: self.handle_size.max(0.0),
                height: self.bounds.height,
            },
            SplitAxis::Vertical => Rect {
                x: self.bounds.x,
                y: self.bounds.y + self.first_extent(),
                width: self.bounds.width,
                height: self.handle_size.max(0.0),
            },
        }
    }

    pub fn second_rect(&self) -> Rect {
        let handle = self.handle_rect();
        match self.axis {
            SplitAxis::Horizontal => Rect {
                x: handle.x + handle.width,
                y: self.bounds.y,
                width: (self.bounds.x + self.bounds.width - (handle.x + handle.width)).max(0.0),
                height: self.bounds.height,
            },
            SplitAxis::Vertical => Rect {
                x: self.bounds.x,
                y: handle.y + handle.height,
                width: self.bounds.width,
                height: (self.bounds.y + self.bounds.height - (handle.y + handle.height)).max(0.0),
            },
        }
    }

    pub fn handle_hit_rect(&self) -> Rect {
        let handle = self.handle_rect();
        let expansion = (self.hit_size.max(handle_primary_size(&handle, self.axis))
            - handle_primary_size(&handle, self.axis))
            * 0.5;
        match self.axis {
            SplitAxis::Horizontal => Rect {
                x: handle.x - expansion,
                y: handle.y,
                width: handle.width + expansion * 2.0,
                height: handle.height,
            },
            SplitAxis::Vertical => Rect {
                x: handle.x,
                y: handle.y - expansion,
                width: handle.width,
                height: handle.height + expansion * 2.0,
            },
        }
    }

    pub fn handle_event(&mut self, event: UiEvent) -> WidgetResponse {
        match event {
            UiEvent::PointerMoved(state) => {
                let hover = self.handle_hit_rect().contains(state.position);
                let mut response = WidgetResponse::default();
                if hover != self.hover {
                    self.hover = hover;
                    response.request_redraw = true;
                }
                if self.dragging && self.drag_state.map(|drag| drag.pointer_id) == Some(state.id) {
                    if self.update_from_pointer(state.position) {
                        response.request_redraw = true;
                    }
                    response.input_consumed = true;
                }
                response
            }
            UiEvent::PointerLeft => {
                if self.hover && !self.dragging {
                    self.hover = false;
                    return WidgetResponse::redraw();
                }
                WidgetResponse::default()
            }
            UiEvent::PointerPressed {
                button: PointerButton::Primary,
                state,
            } => {
                if !self.handle_hit_rect().contains(state.position) {
                    return WidgetResponse::default();
                }
                let handle = self.handle_rect();
                let handle_offset = match self.axis {
                    SplitAxis::Horizontal => state.position.x - handle.x,
                    SplitAxis::Vertical => state.position.y - handle.y,
                };
                self.drag_state = Some(SplitDragState {
                    pointer_id: state.id,
                    handle_offset,
                });
                self.dragging = true;
                self.hover = true;
                let _ = self.update_from_pointer(state.position);
                WidgetResponse::redraw_consumed()
            }
            UiEvent::PointerReleased {
                button: PointerButton::Primary,
                state,
            } => {
                if self.dragging && self.drag_state.map(|drag| drag.pointer_id) == Some(state.id) {
                    let _ = self.update_from_pointer(state.position);
                    self.dragging = false;
                    self.drag_state = None;
                    self.hover = self.handle_hit_rect().contains(state.position);
                    return WidgetResponse::redraw_consumed();
                }
                WidgetResponse::default()
            }
            _ => WidgetResponse::default(),
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

        let handle = self.handle_rect();
        let fill = if self.dragging {
            Color::rgba(0x6f, 0xa2, 0xf3, 0xe0)
        } else if self.hover {
            Color::rgba(0x5c, 0x8d, 0xe8, 0xc8)
        } else {
            Color::rgba(0x58, 0x60, 0x72, 0x8c)
        };
        let border = if self.dragging {
            Color::rgba(0xd8, 0xe5, 0xff, 0xf0)
        } else {
            Color::rgba(0xa5, 0xaa, 0xbe, 0xc8)
        };
        scene.push(PaintOp::FillRect {
            rect: handle,
            color: fill,
        });
        scene.push(PaintOp::StrokeRect {
            rect: handle,
            color: border,
        });
    }

    pub fn clamp_ratio(&mut self) {
        let (min_ratio, max_ratio) = self.ratio_bounds();
        self.split_ratio = self.split_ratio.clamp(min_ratio, max_ratio);
    }

    fn ratio_bounds(&self) -> (f32, f32) {
        let available = self.available_span();
        if available <= f32::EPSILON {
            return (0.5, 0.5);
        }
        let min_ratio = (self.min_first.max(0.0) / available).clamp(0.0, 1.0);
        let max_ratio = (1.0 - self.min_second.max(0.0) / available).clamp(0.0, 1.0);
        if min_ratio > max_ratio {
            let collapsed = 0.5;
            (collapsed, collapsed)
        } else {
            (min_ratio, max_ratio)
        }
    }

    fn first_extent(&self) -> f32 {
        let available = self.available_span();
        if available <= f32::EPSILON {
            0.0
        } else {
            available * self.split_ratio
        }
    }

    fn available_span(&self) -> f32 {
        (primary_span(self.bounds, self.axis) - self.handle_size.max(0.0)).max(0.0)
    }

    fn update_from_pointer(&mut self, point: Point) -> bool {
        let Some(drag) = self.drag_state else {
            return false;
        };
        let axis_point = match self.axis {
            SplitAxis::Horizontal => point.x,
            SplitAxis::Vertical => point.y,
        };
        let axis_origin = match self.axis {
            SplitAxis::Horizontal => self.bounds.x,
            SplitAxis::Vertical => self.bounds.y,
        };
        let available = self.available_span();
        if available <= f32::EPSILON {
            return false;
        }
        let handle_origin = axis_point - drag.handle_offset;
        let first_extent = (handle_origin - axis_origin).clamp(0.0, available);
        let previous = self.split_ratio;
        self.split_ratio = first_extent / available;
        self.clamp_ratio();
        (self.split_ratio - previous).abs() > f32::EPSILON
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SplitNode {
    pub id: i32,
    pub model: SplitNodeModel,
}

impl SplitNode {
    pub fn new(id: i32, axis: SplitAxis, bounds: Rect) -> Self {
        Self {
            id,
            model: SplitNodeModel::new(axis, bounds),
        }
    }
}

impl Component for SplitNode {
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

fn primary_span(rect: Rect, axis: SplitAxis) -> f32 {
    match axis {
        SplitAxis::Horizontal => rect.width,
        SplitAxis::Vertical => rect.height,
    }
}

fn handle_primary_size(rect: &Rect, axis: SplitAxis) -> f32 {
    match axis {
        SplitAxis::Horizontal => rect.width,
        SplitAxis::Vertical => rect.height,
    }
}

#[cfg(test)]
mod tests {
    use super::{SplitAxis, SplitNode, SplitNodeModel};
    use crate::{
        component::Component,
        geometry::{Color, Point, Rect},
        input::{Modifiers, PointerButton, PointerState, UiEvent},
        paint::PaintOp,
        widget::WidgetResponse,
    };

    fn pointer(x: f32, y: f32) -> PointerState {
        PointerState::mouse(Point { x, y }, Modifiers::default())
    }

    #[test]
    fn horizontal_split_rects_follow_ratio_and_handle_size() {
        let mut split = SplitNodeModel::new(
            SplitAxis::Horizontal,
            Rect {
                x: 10.0,
                y: 20.0,
                width: 300.0,
                height: 120.0,
            },
        );
        split.handle_size = 10.0;
        split.set_split_ratio(0.25);

        assert_eq!(
            split.first_rect(),
            Rect {
                x: 10.0,
                y: 20.0,
                width: 72.5,
                height: 120.0,
            }
        );
        assert_eq!(
            split.handle_rect(),
            Rect {
                x: 82.5,
                y: 20.0,
                width: 10.0,
                height: 120.0,
            }
        );
        assert_eq!(
            split.second_rect(),
            Rect {
                x: 92.5,
                y: 20.0,
                width: 217.5,
                height: 120.0,
            }
        );
    }

    #[test]
    fn vertical_split_rects_follow_ratio_and_handle_size() {
        let mut split = SplitNodeModel::new(
            SplitAxis::Vertical,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 400.0,
            },
        );
        split.handle_size = 12.0;
        split.set_split_ratio(0.5);

        assert_eq!(split.first_rect().height, 194.0);
        assert_eq!(split.handle_rect().y, 194.0);
        assert_eq!(split.handle_rect().height, 12.0);
        assert_eq!(split.second_rect().y, 206.0);
        assert_eq!(split.second_rect().height, 194.0);
    }

    #[test]
    fn split_ratio_clamps_to_minimum_pane_sizes() {
        let mut split = SplitNodeModel::new(
            SplitAxis::Horizontal,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 300.0,
                height: 80.0,
            },
        );
        split.handle_size = 10.0;
        split.min_first = 120.0;
        split.min_second = 100.0;
        split.set_split_ratio(0.05);
        assert!((split.first_rect().width - 120.0).abs() < 0.001);

        split.set_split_ratio(0.95);
        assert!((split.second_rect().width - 100.0).abs() < 0.001);
    }

    #[test]
    fn split_handle_drag_updates_ratio_in_logical_space() {
        let mut split = SplitNodeModel::new(
            SplitAxis::Horizontal,
            Rect {
                x: 20.0,
                y: 10.0,
                width: 420.0,
                height: 60.0,
            },
        );
        split.handle_size = 10.0;
        split.min_first = 80.0;
        split.min_second = 80.0;
        split.set_split_ratio(0.5);

        let handle = split.handle_rect();
        let press = split.handle_event(UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state: pointer(handle.x + 5.0, handle.y + 12.0),
        });
        assert!(press.request_redraw);
        assert!(press.input_consumed);
        assert!(split.dragging);

        let moved = split.handle_event(UiEvent::PointerMoved(pointer(120.0, handle.y + 12.0)));
        assert!(moved.request_redraw);
        assert!(moved.input_consumed);
        assert!(split.first_rect().width >= 80.0);

        let release = split.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: pointer(120.0, handle.y + 12.0),
        });
        assert!(release.request_redraw);
        assert!(release.input_consumed);
        assert!(!split.dragging);
    }

    #[test]
    fn split_handle_hit_rect_expands_beyond_visual_handle() {
        let mut split = SplitNodeModel::new(
            SplitAxis::Horizontal,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 320.0,
                height: 100.0,
            },
        );
        split.handle_size = 6.0;
        split.hit_size = 18.0;

        let handle = split.handle_rect();
        let hit = split.handle_hit_rect();
        assert!(hit.width > handle.width);
        assert!(hit.x < handle.x);
        assert!(hit.x + hit.width > handle.x + handle.width);
    }

    #[test]
    fn split_press_outside_handle_does_not_start_drag() {
        let mut split = SplitNodeModel::new(
            SplitAxis::Horizontal,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 320.0,
                height: 100.0,
            },
        );
        let response = split.handle_event(UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state: pointer(8.0, 8.0),
        });

        assert_eq!(response, WidgetResponse::default());
        assert!(!split.dragging);
        assert!(split.drag_state.is_none());
    }

    #[test]
    fn split_pointer_left_clears_hover_when_not_dragging() {
        let mut split = SplitNodeModel::new(
            SplitAxis::Horizontal,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 320.0,
                height: 100.0,
            },
        );
        let handle = split.handle_rect();
        let moved = split.handle_event(UiEvent::PointerMoved(pointer(handle.x + 1.0, 12.0)));
        assert!(moved.request_redraw);
        assert!(split.hover);

        let left = split.handle_event(UiEvent::PointerLeft);
        assert!(left.request_redraw);
        assert!(!split.hover);
    }

    #[test]
    fn split_impossible_minimums_collapse_to_stable_midpoint() {
        let mut split = SplitNodeModel::new(
            SplitAxis::Horizontal,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 80.0,
            },
        );
        split.handle_size = 10.0;
        split.min_first = 90.0;
        split.min_second = 90.0;
        split.set_split_ratio(0.1);

        assert!((split.split_ratio - 0.5).abs() < 0.001);
        assert!((split.first_rect().width - 55.0).abs() < 0.001);
        assert!((split.second_rect().width - 55.0).abs() < 0.001);
    }

    #[test]
    fn vertical_split_drag_clamps_against_second_minimum() {
        let mut split = SplitNodeModel::new(
            SplitAxis::Vertical,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 300.0,
            },
        );
        split.handle_size = 12.0;
        split.min_first = 40.0;
        split.min_second = 100.0;
        split.set_split_ratio(0.5);

        let handle = split.handle_rect();
        let _ = split.handle_event(UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state: pointer(10.0, handle.y + 4.0),
        });
        let _ = split.handle_event(UiEvent::PointerMoved(pointer(10.0, 280.0)));
        let _ = split.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: pointer(10.0, 280.0),
        });

        assert!(split.second_rect().height >= 100.0);
    }

    #[test]
    fn split_paint_emits_chrome_and_handle() {
        let mut split = SplitNodeModel::new(
            SplitAxis::Horizontal,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 240.0,
                height: 120.0,
            },
        );
        split.background = Some(Color::rgba(10, 20, 30, 255));
        split.border = Some(Color::rgba(200, 210, 220, 255));

        let mut ops = Vec::new();
        split.paint(&mut ops);

        assert!(matches!(ops[0], PaintOp::FillRect { .. }));
        assert!(matches!(ops[1], PaintOp::StrokeRect { .. }));
        assert!(matches!(ops[2], PaintOp::FillRect { .. }));
        assert!(matches!(ops[3], PaintOp::StrokeRect { .. }));
    }

    #[test]
    fn split_component_updates_bounds() {
        let mut split = SplitNode::new(
            77,
            SplitAxis::Vertical,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 80.0,
                height: 90.0,
            },
        );
        split.set_bounds(Rect {
            x: 6.0,
            y: 7.0,
            width: 140.0,
            height: 220.0,
        });
        assert_eq!(
            split.bounds(),
            Rect {
                x: 6.0,
                y: 7.0,
                width: 140.0,
                height: 220.0,
            }
        );
    }
}
