use crate::{
    geometry::{Color, Insets, Point, Rect},
    paint::PaintOp,
    scroll::{ScrollRegionModel, ScrollThumbDragState},
    text::{multiline_line_step, single_line_text_box_height},
    text_block::TextBlockModel,
};

#[derive(Debug, Clone, PartialEq)]
pub struct DetailsSection {
    pub text: String,
    pub color: Color,
    pub font_size: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DetailsViewModel {
    pub bounds: Rect,
    pub padding: Insets,
    pub gap: f32,
    pub scrollbar_gutter: f32,
    pub scroll_region: ScrollRegionModel,
    pub drag_state: Option<ScrollThumbDragState>,
}

impl DetailsViewModel {
    pub fn new(bounds: Rect) -> Self {
        Self {
            bounds,
            padding: Insets::default(),
            gap: 0.0,
            scrollbar_gutter: 18.0,
            scroll_region: ScrollRegionModel::new(bounds, 0.0),
            drag_state: None,
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

    pub fn text_column_rect(&self) -> Rect {
        let content = self.content_rect();
        Rect {
            x: content.x,
            y: content.y,
            width: (content.width - self.scrollbar_gutter).max(1.0),
            height: content.height,
        }
    }

    pub fn set_section_heights(&mut self, section_heights: &[f32]) -> f32 {
        let content_height = section_heights.iter().copied().sum::<f32>()
            + self.gap * section_heights.len().saturating_sub(1) as f32;
        self.scroll_region.set_viewport(self.content_rect());
        self.scroll_region.set_content_height(content_height);
        content_height
    }

    pub fn layout_section_rects(&self, section_heights: &[f32]) -> Vec<Rect> {
        let text_column = self.text_column_rect();
        let mut y = self.scroll_region.content_origin_y(0.0);
        let mut rects = Vec::with_capacity(section_heights.len());
        for height in section_heights {
            rects.push(Rect {
                x: text_column.x,
                y,
                width: text_column.width,
                height: *height,
            });
            y += *height + self.gap;
        }
        rects
    }

    pub fn visible(&self, y: f32, height: f32) -> bool {
        self.scroll_region.visible(y, height)
    }

    pub fn measure_text_section_heights<F>(
        &self,
        sections: &[DetailsSection],
        mut measure_width: F,
    ) -> Vec<f32>
    where
        F: FnMut(&str, u16) -> f32,
    {
        let max_width = self.text_column_rect().width.max(8.0);
        sections
            .iter()
            .map(|section| {
                let lines = wrap_text_to_width(
                    &section.text,
                    max_width,
                    section.font_size,
                    &mut measure_width,
                );
                let line_box_height = single_line_text_box_height(section.font_size);
                let line_height = multiline_line_step(section.font_size);
                let block_height =
                    line_box_height + (lines.len().saturating_sub(1) as f32 * line_height);
                block_height.max(line_box_height)
            })
            .collect()
    }

    pub fn apply_input(
        &mut self,
        pointer: Point,
        pointer_in_bounds: bool,
        mouse_wheel_y: f32,
        mouse_pressed: bool,
        mouse_down: bool,
        scroll_delta: f32,
    ) {
        if pointer_in_bounds && mouse_wheel_y.abs() > f32::EPSILON {
            self.scroll_region
                .apply_scroll_delta(-mouse_wheel_y * scroll_delta);
        }

        if let Some(active_drag) = self.drag_state {
            if mouse_down {
                self.scroll_region.drag_indicator_to(pointer.y, active_drag);
                return;
            }
            self.drag_state = None;
            return;
        }

        let Some(track) = self.scroll_region.indicator_track_rect() else {
            self.drag_state = None;
            return;
        };
        if !mouse_pressed {
            return;
        }
        let interactive_rect = Rect {
            x: track.x - 8.0,
            y: track.y,
            width: track.width + 12.0,
            height: track.height,
        };
        if !interactive_rect.contains(pointer) {
            return;
        }
        if let Some(drag) = self.scroll_region.begin_indicator_drag(pointer.y) {
            self.drag_state = Some(drag);
            self.scroll_region.drag_indicator_to(pointer.y, drag);
        } else {
            self.scroll_region.scroll_to_indicator_position(pointer.y);
        }
    }

    pub fn clip_text_paint_ops(&self, ops: &mut [PaintOp]) {
        let clip_rect = self.text_column_rect();
        for op in ops {
            if let PaintOp::Text {
                clip_rect: existing_clip,
                ..
            } = op
            {
                *existing_clip = Some(intersect_rect(
                    existing_clip.unwrap_or(clip_rect),
                    clip_rect,
                ));
            }
        }
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>, indicator_color: Color) {
        self.scroll_region.paint_indicator(scene, indicator_color);
    }

    pub fn paint_text_sections<F>(
        &self,
        scene: &mut Vec<PaintOp>,
        sections: &[DetailsSection],
        section_heights: &[f32],
        mut measure_width: F,
    ) where
        F: FnMut(&str, u16) -> f32,
    {
        let rects = self.layout_section_rects(section_heights);
        let max_width = self.text_column_rect().width.max(8.0);
        for (section, rect) in sections.iter().zip(rects) {
            if !self.visible(rect.y, rect.height) {
                continue;
            }
            let block = build_text_block_model(
                &section.text,
                rect,
                section.color,
                section.font_size,
                max_width,
                &mut measure_width,
            );
            let start = scene.len();
            block.paint(scene);
            self.clip_text_paint_ops(&mut scene[start..]);
        }
    }
}

fn intersect_rect(a: Rect, b: Rect) -> Rect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    Rect {
        x,
        y,
        width: (right - x).max(0.0),
        height: (bottom - y).max(0.0),
    }
}

fn build_text_block_model<F>(
    text: &str,
    bounds: Rect,
    color: Color,
    font_size: u16,
    max_width: f32,
    measure_width: &mut F,
) -> TextBlockModel
where
    F: FnMut(&str, u16) -> f32,
{
    let lines = wrap_text_to_width(text, max_width, font_size, measure_width);
    let mut block = TextBlockModel::new(lines.join("\n"), bounds);
    block.style.color = color;
    block.style.font_size = font_size;
    block
}

fn wrap_text_to_width<F>(
    text: &str,
    max_width: f32,
    font_size: u16,
    measure_width: &mut F,
) -> Vec<String>
where
    F: FnMut(&str, u16) -> f32,
{
    fn push_wrapped_token<F>(
        lines: &mut Vec<String>,
        current_line: &mut String,
        token: &str,
        max_width: f32,
        font_size: u16,
        measure_width: &mut F,
    ) where
        F: FnMut(&str, u16) -> f32,
    {
        if token.is_empty() {
            return;
        }
        let token_width = measure_width(token, font_size);
        if token_width <= max_width {
            if current_line.is_empty() {
                current_line.push_str(token);
            } else {
                let candidate = format!("{current_line} {token}");
                if measure_width(&candidate, font_size) <= max_width {
                    *current_line = candidate;
                } else {
                    lines.push(std::mem::take(current_line));
                    current_line.push_str(token);
                }
            }
            return;
        }

        if !current_line.is_empty() {
            lines.push(std::mem::take(current_line));
        }

        let mut chunk = String::new();
        for ch in token.chars() {
            let candidate = format!("{chunk}{ch}");
            if !chunk.is_empty() && measure_width(&candidate, font_size) > max_width {
                lines.push(std::mem::take(&mut chunk));
            }
            chunk.push(ch);
        }
        if !chunk.is_empty() {
            current_line.push_str(&chunk);
        }
    }

    let mut lines = Vec::new();
    for raw_line in text.split('\n') {
        if raw_line.trim().is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut current_line = String::new();
        for word in raw_line.split_whitespace() {
            push_wrapped_token(
                &mut lines,
                &mut current_line,
                word,
                max_width,
                font_size,
                measure_width,
            );
        }

        if !current_line.is_empty() {
            lines.push(current_line);
        }
    }

    if lines.is_empty() {
        lines.push(String::new());
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::DetailsViewModel;
    use crate::{
        geometry::{Color, Insets, Point, Rect},
        paint::{PaintOp, TextStyle},
    };

    #[test]
    fn text_column_rect_reserves_scrollbar_gutter() {
        let mut view = DetailsViewModel::new(Rect {
            x: 20.0,
            y: 30.0,
            width: 240.0,
            height: 160.0,
        });
        view.padding = Insets {
            left: 12.0,
            top: 8.0,
            right: 12.0,
            bottom: 10.0,
        };

        assert_eq!(
            view.text_column_rect(),
            Rect {
                x: 32.0,
                y: 38.0,
                width: 198.0,
                height: 142.0,
            }
        );
    }

    #[test]
    fn layout_section_rects_follow_gap_and_offset() {
        let mut view = DetailsViewModel::new(Rect {
            x: 20.0,
            y: 30.0,
            width: 240.0,
            height: 120.0,
        });
        view.padding = Insets {
            left: 12.0,
            top: 8.0,
            right: 12.0,
            bottom: 10.0,
        };
        view.gap = 6.0;
        view.set_section_heights(&[60.0, 50.0, 40.0]);
        view.scroll_region.apply_scroll_delta(18.0);

        let rects = view.layout_section_rects(&[60.0, 50.0, 40.0]);

        assert_eq!(rects[0].y, 20.0);
        assert_eq!(rects[1].y, 86.0);
        assert_eq!(rects[2].y, 142.0);
    }

    #[test]
    fn apply_input_preserves_offset_and_supports_thumb_drag() {
        let mut view = DetailsViewModel::new(Rect {
            x: 20.0,
            y: 30.0,
            width: 240.0,
            height: 120.0,
        });
        view.padding = Insets {
            left: 12.0,
            top: 8.0,
            right: 12.0,
            bottom: 10.0,
        };
        view.set_section_heights(&[220.0]);

        view.apply_input(Point { x: 80.0, y: 80.0 }, true, -2.0, false, false, 21.0);
        assert_eq!(view.scroll_region.offset, 42.0);

        let thumb = view.scroll_region.indicator_thumb_rect().expect("thumb");
        let pointer = Point {
            x: thumb.x + thumb.width * 0.5,
            y: thumb.y + thumb.height * 0.5,
        };
        view.apply_input(pointer, true, 0.0, true, true, 21.0);
        assert!(view.drag_state.is_some());

        let drag_pointer = Point {
            x: pointer.x,
            y: pointer.y + 20.0,
        };
        view.apply_input(drag_pointer, true, 0.0, false, true, 21.0);
        assert!(view.scroll_region.offset > 42.0);
    }

    #[test]
    fn clip_text_paint_ops_clamps_to_text_column() {
        let mut view = DetailsViewModel::new(Rect {
            x: 20.0,
            y: 30.0,
            width: 240.0,
            height: 120.0,
        });
        view.padding = Insets {
            left: 12.0,
            top: 8.0,
            right: 12.0,
            bottom: 10.0,
        };
        let mut ops = vec![PaintOp::Text {
            rect: Rect {
                x: 32.0,
                y: 38.0,
                width: 198.0,
                height: 180.0,
            },
            clip_rect: Some(Rect {
                x: 32.0,
                y: 38.0,
                width: 198.0,
                height: 180.0,
            }),
            text: "hello".to_string(),
            style: TextStyle {
                color: Color::rgba(255, 255, 255, 255),
                ..TextStyle::default()
            },
        }];

        view.clip_text_paint_ops(&mut ops);

        let PaintOp::Text { clip_rect, .. } = &ops[0] else {
            panic!("expected text op");
        };
        assert_eq!(
            *clip_rect,
            Some(Rect {
                x: 32.0,
                y: 38.0,
                width: 198.0,
                height: 102.0,
            })
        );
    }
}
