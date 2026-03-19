use crate::{
    component::Component,
    geometry::{Color, Insets, Point, Rect},
    input::{Key, PointerButton, UiEvent},
    paint::{HorizontalAlign, PaintOp, TextLayoutMode, TextOverflow, TextStyle, VerticalAlign},
    scroll::{ScrollbarAxis, ScrollbarDragState, ScrollbarModel},
    text::single_line_text_box_height,
    widget::{WidgetId, WidgetResponse},
};

#[derive(Debug, Clone, PartialEq)]
pub struct TextAreaLineLayout {
    pub source_start: usize,
    pub source_end: usize,
    pub display_text: String,
    pub rect: Rect,
    pub char_offsets: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextAreaLayoutCache {
    pub lines: Vec<TextAreaLineLayout>,
    pub selection_rects: Vec<Rect>,
    pub caret_rect: Option<Rect>,
    pub content_rect: Rect,
    pub content_width: f32,
    pub content_height: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextAreaModel {
    pub widget_id: WidgetId,
    pub bounds: Rect,
    pub text: String,
    pub style: TextStyle,
    pub padding: Insets,
    pub background: Option<Color>,
    pub border: Option<Color>,
    pub selection_fill: Color,
    pub caret_color: Color,
    pub line_spacing: f32,
    pub scroll_x: f32,
    pub scroll_y: f32,
    pub focused: bool,
    pub show_caret: bool,
    pub hover: bool,
    pub drag_selecting: bool,
    pub horizontal_drag: Option<ScrollbarDragState>,
    pub vertical_drag: Option<ScrollbarDragState>,
    pub caret_visibility_pending: bool,
    pub selection_anchor: usize,
    pub selection_head: usize,
    pub preferred_x: Option<f32>,
    pub tab_spaces: usize,
    pub layout_cache: TextAreaLayoutCache,
    pub horizontal_scrollbar: ScrollbarModel,
    pub vertical_scrollbar: ScrollbarModel,
}

impl TextAreaModel {
    const HSCROLL_HEIGHT: f32 = 10.0;
    const HSCROLL_GAP: f32 = 4.0;
    const HSCROLL_HIT_PAD_X: f32 = 2.0;
    const HSCROLL_HIT_PAD_Y: f32 = 6.0;
    const VSCROLL_WIDTH: f32 = 10.0;
    const VSCROLL_GAP: f32 = 4.0;
    const VSCROLL_HIT_PAD_X: f32 = 6.0;
    const VSCROLL_HIT_PAD_Y: f32 = 2.0;

    pub fn new(text: impl Into<String>, bounds: Rect) -> Self {
        let mut style = TextStyle::default();
        style.layout_mode = TextLayoutMode::MultiLine;
        style.horizontal_align = HorizontalAlign::Left;
        style.vertical_align = VerticalAlign::Top;
        style.vertical_metric_mode = crate::TextVerticalMetricMode::LogicalLineBox;
        style.overflow = TextOverflow::Clip;
        style.color = Color::rgba(0xeb, 0xef, 0xf7, 0xff);
        Self {
            widget_id: WidgetId(0),
            bounds,
            text: text.into(),
            style,
            padding: Insets {
                left: 12.0,
                top: 10.0,
                right: 12.0,
                bottom: 10.0,
            },
            background: Some(Color::rgba(0x14, 0x19, 0x22, 0xf2)),
            border: Some(Color::rgba(0x5f, 0x6b, 0x80, 0xff)),
            selection_fill: Color::rgba(0x39, 0x5e, 0x96, 0xd8),
            caret_color: Color::rgba(0xf4, 0xf6, 0xfa, 0xff),
            line_spacing: 2.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            focused: false,
            show_caret: true,
            hover: false,
            drag_selecting: false,
            horizontal_drag: None,
            vertical_drag: None,
            caret_visibility_pending: true,
            selection_anchor: 0,
            selection_head: 0,
            preferred_x: None,
            tab_spaces: 4,
            layout_cache: TextAreaLayoutCache::default(),
            horizontal_scrollbar: ScrollbarModel::new(
                ScrollbarAxis::Horizontal,
                Rect::default(),
                0.0,
            ),
            vertical_scrollbar: ScrollbarModel::new(
                ScrollbarAxis::Vertical,
                Rect::default(),
                0.0,
            ),
        }
    }

    pub fn with_id(widget_id: WidgetId, text: impl Into<String>, bounds: Rect) -> Self {
        let mut area = Self::new(text, bounds);
        area.widget_id = widget_id;
        area
    }

    pub fn set_bounds(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    pub fn content_rect(&self) -> Rect {
        let base = self.base_content_rect();
        if self.horizontal_scrollbar.is_scrollable() || self.vertical_scrollbar.is_scrollable() {
            Rect {
                x: base.x,
                y: base.y,
                width: (base.width
                    - if self.vertical_scrollbar.is_scrollable() {
                        self.vertical_scrollbar_total_width()
                    } else {
                        0.0
                    })
                .max(0.0),
                height: (base.height
                    - if self.horizontal_scrollbar.is_scrollable() {
                        self.horizontal_scrollbar_total_height()
                    } else {
                        0.0
                    })
                .max(0.0),
            }
        } else {
            base
        }
    }

    fn base_content_rect(&self) -> Rect {
        Rect {
            x: self.bounds.x + self.padding.left,
            y: self.bounds.y + self.padding.top,
            width: (self.bounds.width - self.padding.left - self.padding.right).max(0.0),
            height: (self.bounds.height - self.padding.top - self.padding.bottom).max(0.0),
        }
    }

    pub fn caret(&self) -> usize {
        self.selection_head
    }

    pub fn prefers_text_cursor(&self, point: Point) -> bool {
        self.layout_cache.content_rect.contains(point)
            && self
                .horizontal_scrollbar
                .interactive_rect(Self::HSCROLL_HIT_PAD_Y, Self::HSCROLL_HIT_PAD_X)
                .is_none_or(|track| !track.contains(point))
            && self
                .vertical_scrollbar
                .interactive_rect(Self::VSCROLL_HIT_PAD_X, Self::VSCROLL_HIT_PAD_Y)
                .is_none_or(|track| !track.contains(point))
    }

    pub fn selection_range(&self) -> Option<(usize, usize)> {
        if self.selection_anchor == self.selection_head {
            None
        } else {
            Some(ordered_range(self.selection_anchor, self.selection_head))
        }
    }

    pub fn select_all(&mut self) {
        let len = self.char_len();
        self.selection_anchor = 0;
        self.selection_head = len;
        self.preferred_x = None;
        self.request_caret_visibility();
    }

    pub fn relayout<F>(&mut self, mut measure_width: F)
    where
        F: FnMut(&str, u16) -> f32,
    {
        self.relayout_internal(&mut measure_width);
        if self.caret_visibility_pending {
            let previous_scroll_x = self.scroll_x;
            let previous_scroll_y = self.scroll_y;
            self.ensure_caret_visible();
            self.caret_visibility_pending = false;
            if (self.scroll_x - previous_scroll_x).abs() > f32::EPSILON
                || (self.scroll_y - previous_scroll_y).abs() > f32::EPSILON
            {
                self.relayout_internal(&mut measure_width);
            }
        }
    }

    fn relayout_internal<F>(&mut self, measure_width: &mut F)
    where
        F: FnMut(&str, u16) -> f32,
    {
        let base_content_rect = self.base_content_rect();
        let line_box_height = single_line_text_box_height(self.style.font_size);
        let line_step = line_box_height + self.line_spacing.max(0.0);
        let line_ranges = self.line_ranges();
        let mut lines = Vec::with_capacity(line_ranges.len());
        let mut selection_rects = Vec::new();
        let selection_range = self.selection_range();
        let mut caret_rect = None;
        let caret = self.caret();
        let mut content_width: f32 = 0.0;

        for (index, (source_start, source_end)) in line_ranges.into_iter().enumerate() {
            let y = base_content_rect.y + index as f32 * line_step - self.scroll_y;
            let rect = Rect {
                x: base_content_rect.x,
                y,
                width: base_content_rect.width,
                height: line_box_height,
            };
            let source_text = self.slice_chars(source_start, source_end);
            let (display_text, char_offsets) = build_display_text(
                &source_text,
                self.tab_spaces,
                self.style.font_size,
                measure_width,
            );

            let line = TextAreaLineLayout {
                source_start,
                source_end,
                display_text,
                rect,
                char_offsets,
            };
            let line_width = line.char_offsets.last().copied().unwrap_or(0.0);
            content_width = content_width.max(line_width);

            if let Some((selection_start, selection_end)) = selection_range {
                if selection_start < line.source_end && selection_end > line.source_start {
                    let local_start = selection_start
                        .saturating_sub(line.source_start)
                        .min(line.char_offsets.len().saturating_sub(1));
                    let local_end = selection_end
                        .saturating_sub(line.source_start)
                        .min(line.char_offsets.len().saturating_sub(1));
                    if local_end > local_start {
                        selection_rects.push(Rect {
                            x: line.rect.x + line.char_offsets[local_start] - self.scroll_x,
                            y: line.rect.y,
                            width: (line.char_offsets[local_end] - line.char_offsets[local_start])
                                .max(1.0),
                            height: line.rect.height,
                        });
                    }
                }
            }

            if caret >= line.source_start && caret <= line.source_end {
                let local = caret
                    .saturating_sub(line.source_start)
                    .min(line.char_offsets.len().saturating_sub(1));
                let caret_top = line.rect.y + 1.0;
                let caret_height =
                    (self.style.font_size as f32 + 1.0).min((line.rect.height - 2.0).max(1.0));
                caret_rect = Some(Rect {
                    x: line.rect.x + line.char_offsets[local] - self.scroll_x,
                    y: caret_top,
                    width: 1.5,
                    height: caret_height,
                });
            }

            lines.push(line);
        }

        if caret_rect.is_none() && lines.is_empty() {
            let caret_height =
                (self.style.font_size as f32 + 1.0).min((line_box_height - 2.0).max(1.0));
            caret_rect = Some(Rect {
                x: base_content_rect.x - self.scroll_x,
                y: base_content_rect.y + 1.0,
                width: 1.5,
                height: caret_height,
            });
        }

        let content_height = if lines.is_empty() {
            line_box_height
        } else {
            ((lines.len() - 1) as f32 * line_step + line_box_height).max(line_box_height)
        };
        let content_rect = self.layout_scrollbars(base_content_rect, content_width, content_height);
        self.layout_cache = TextAreaLayoutCache {
            lines,
            selection_rects,
            caret_rect,
            content_rect,
            content_width,
            content_height,
        };
        self.clamp_scroll();
    }

    pub fn handle_event(&mut self, event: UiEvent) -> WidgetResponse {
        match event {
            UiEvent::PointerMoved(state) => {
                if let Some(drag) = self.horizontal_drag {
                    self.drag_horizontal_scroll_to(state.position.x, drag);
                    let mut response = WidgetResponse::redraw_consumed();
                    response.request_focus = false;
                    return response;
                }
                if let Some(drag) = self.vertical_drag {
                    self.drag_vertical_scroll_to(state.position.y, drag);
                    let mut response = WidgetResponse::redraw_consumed();
                    response.request_focus = false;
                    return response;
                }
                let hover = self.bounds.contains(state.position);
                let mut response = WidgetResponse::default();
                if hover != self.hover {
                    self.hover = hover;
                    response.request_redraw = true;
                }
                if self.drag_selecting {
                    let caret = self.hit_test(state.position);
                    if caret != self.selection_head {
                        self.selection_head = caret;
                        self.preferred_x = None;
                        self.request_caret_visibility();
                        response.request_redraw = true;
                    }
                    response.input_consumed = true;
                }
                response
            }
            UiEvent::PointerLeft => {
                if self.hover {
                    self.hover = false;
                    return WidgetResponse::redraw();
                }
                WidgetResponse::default()
            }
            UiEvent::PointerPressed {
                button: PointerButton::Primary,
                state,
            } => {
                if let Some(track) = self
                    .vertical_scrollbar
                    .interactive_rect(Self::VSCROLL_HIT_PAD_X, Self::VSCROLL_HIT_PAD_Y)
                {
                    if track.contains(state.position) {
                        self.focused = true;
                        if let Some(drag) = self.vertical_scrollbar.begin_indicator_drag(state.position)
                        {
                            self.vertical_drag = Some(drag);
                            self.vertical_scrollbar.drag_indicator_to(state.position, drag);
                            self.sync_scroll_offsets_from_scrollbars();
                        } else {
                            self.vertical_scrollbar
                                .scroll_to_indicator_position(state.position);
                            self.sync_scroll_offsets_from_scrollbars();
                        }
                        return WidgetResponse {
                            request_redraw: true,
                            request_focus: true,
                            input_consumed: true,
                            action: None,
                        };
                    }
                }
                if let Some(track) = self
                    .horizontal_scrollbar
                    .interactive_rect(Self::HSCROLL_HIT_PAD_Y, Self::HSCROLL_HIT_PAD_X)
                {
                    if track.contains(state.position) {
                        self.focused = true;
                        if let Some(drag) =
                            self.horizontal_scrollbar.begin_indicator_drag(state.position)
                        {
                            self.horizontal_drag = Some(drag);
                            self.horizontal_scrollbar
                                .drag_indicator_to(state.position, drag);
                            self.sync_scroll_offsets_from_scrollbars();
                        } else {
                            self.horizontal_scrollbar
                                .scroll_to_indicator_position(state.position);
                            self.sync_scroll_offsets_from_scrollbars();
                        }
                        return WidgetResponse {
                            request_redraw: true,
                            request_focus: true,
                            input_consumed: true,
                            action: None,
                        };
                    }
                }
                if !self.bounds.contains(state.position) {
                    return WidgetResponse::default();
                }
                let caret = self.hit_test(state.position);
                self.focused = true;
                self.drag_selecting = true;
                if state.modifiers.shift {
                    self.selection_head = caret;
                } else {
                    self.selection_anchor = caret;
                    self.selection_head = caret;
                }
                self.preferred_x = None;
                self.request_caret_visibility();
                WidgetResponse {
                    request_redraw: true,
                    request_focus: true,
                    input_consumed: true,
                    action: None,
                }
            }
            UiEvent::PointerReleased {
                button: PointerButton::Primary,
                ..
            } => {
                let h = self.horizontal_drag.take().is_some();
                let v = self.vertical_drag.take().is_some();
                if h || v {
                    return WidgetResponse::redraw_consumed();
                }
                if self.drag_selecting {
                    self.drag_selecting = false;
                    return WidgetResponse::redraw_consumed();
                }
                WidgetResponse::default()
            }
            UiEvent::FocusChanged(focused) => {
                if self.focused != focused {
                    self.focused = focused;
                    if !focused {
                        self.drag_selecting = false;
                    }
                    return WidgetResponse::redraw();
                }
                WidgetResponse::default()
            }
            UiEvent::ScrollLines { delta } => {
                let changed = self.scroll_vertical(
                    -(delta as f32)
                        * (single_line_text_box_height(self.style.font_size)
                            + self.line_spacing.max(0.0)),
                );
                if changed {
                    self.sync_scroll_offsets_from_scrollbars();
                    self.clamp_scroll();
                    self.caret_visibility_pending = false;
                    return WidgetResponse::redraw_consumed();
                }
                WidgetResponse::default()
            }
            UiEvent::TextInput { text } => {
                if !self.focused || text.is_empty() {
                    return WidgetResponse::default();
                }
                self.replace_selection(&text);
                self.preferred_x = None;
                self.request_caret_visibility();
                WidgetResponse::redraw_consumed()
            }
            UiEvent::KeyPressed { key, modifiers } => self.handle_key(key, modifiers),
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
        let content = self.layout_cache.content_rect;
        for rect in self.layout_cache.selection_rects.iter().copied() {
            if rect_intersects(rect, content) {
                scene.push(PaintOp::FillRect {
                    rect,
                    color: self.selection_fill,
                });
            }
        }
        for line in &self.layout_cache.lines {
            if !line.display_text.is_empty() && rect_intersects(line.rect, content) {
                scene.push(PaintOp::Text {
                    rect: Rect {
                        x: line.rect.x - self.scroll_x,
                        y: line.rect.y,
                        width: (line.rect.width + self.scroll_x).max(line.rect.width),
                        height: line.rect.height,
                    },
                    clip_rect: Some(content),
                    text: line.display_text.clone(),
                    style: TextStyle {
                        horizontal_align: HorizontalAlign::Left,
                        vertical_align: VerticalAlign::Top,
                        layout_mode: TextLayoutMode::SingleLine,
                        overflow: TextOverflow::Clip,
                        ..self.style.clone()
                    },
                });
            }
        }
        if self.focused && self.show_caret {
            if let Some(caret) = self.layout_cache.caret_rect {
                if rect_intersects(caret, content) {
                    scene.push(PaintOp::Line {
                        from: Point {
                            x: caret.x,
                            y: caret.y,
                        },
                        to: Point {
                            x: caret.x,
                            y: caret.y + caret.height,
                        },
                        color: self.caret_color,
                    });
                }
            }
        }
        self.vertical_scrollbar.paint_indicator(
            scene,
            Color::rgba(30, 36, 52, 220),
            Color::rgba(92, 141, 232, 230),
        );
        self.horizontal_scrollbar.paint_indicator(
            scene,
            Color::rgba(30, 36, 52, 220),
            Color::rgba(92, 141, 232, 230),
        );
    }

    fn handle_key(&mut self, key: Key, modifiers: crate::Modifiers) -> WidgetResponse {
        if !self.focused {
            return WidgetResponse::default();
        }

        if matches!(key, Key::Character('a') | Key::Character('A'))
            && (modifiers.ctrl || modifiers.meta)
        {
            self.select_all();
            return WidgetResponse::redraw_consumed();
        }

        let changed = match key {
            Key::Left => self.move_horizontal(-1, modifiers.shift),
            Key::Right => self.move_horizontal(1, modifiers.shift),
            Key::Up => self.move_vertical(-1, modifiers.shift),
            Key::Down => self.move_vertical(1, modifiers.shift),
            Key::Home => self.move_line_boundary(true, modifiers.shift),
            Key::End => self.move_line_boundary(false, modifiers.shift),
            Key::Backspace => self.backspace(),
            Key::Delete => self.delete_forward(),
            Key::Enter => self.insert_control_text("\n"),
            Key::Tab => self.insert_control_text("\t"),
            _ => false,
        };

        if changed {
            self.request_caret_visibility();
            WidgetResponse::redraw_consumed()
        } else {
            WidgetResponse::default()
        }
    }

    fn move_horizontal(&mut self, delta: isize, extend_selection: bool) -> bool {
        if !extend_selection {
            if let Some((start, end)) = self.selection_range() {
                let next = if delta < 0 { start } else { end };
                self.selection_anchor = next;
                self.selection_head = next;
                self.preferred_x = None;
                return true;
            }
        }
        let len = self.char_len();
        let next = (self.selection_head as isize + delta).clamp(0, len as isize) as usize;
        if next == self.selection_head
            && (!extend_selection || self.selection_anchor == self.selection_head)
        {
            return false;
        }
        if extend_selection {
            self.selection_head = next;
        } else {
            self.selection_anchor = next;
            self.selection_head = next;
        }
        self.preferred_x = None;
        true
    }

    fn move_vertical(&mut self, direction: isize, extend_selection: bool) -> bool {
        let Some((line_index, _local_column, caret_x)) = self.line_position_for_caret() else {
            return false;
        };
        let next_line_index = (line_index as isize + direction)
            .clamp(0, self.layout_cache.lines.len().saturating_sub(1) as isize)
            as usize;
        if next_line_index == line_index && direction != 0 {
            return false;
        }
        let preferred_x = self.preferred_x.unwrap_or(caret_x);
        let next_line = &self.layout_cache.lines[next_line_index];
        let next_local = closest_char_offset_index(&next_line.char_offsets, preferred_x)
            .min(next_line.source_end.saturating_sub(next_line.source_start));
        let next_caret = next_line.source_start + next_local;
        if extend_selection {
            self.selection_head = next_caret;
        } else {
            self.selection_anchor = next_caret;
            self.selection_head = next_caret;
        }
        self.preferred_x = Some(preferred_x);
        true
    }

    fn move_line_boundary(&mut self, to_start: bool, extend_selection: bool) -> bool {
        let Some((line_index, _, _)) = self.line_position_for_caret() else {
            return false;
        };
        let line = &self.layout_cache.lines[line_index];
        let next = if to_start {
            line.source_start
        } else {
            line.source_end
        };
        if extend_selection {
            self.selection_head = next;
        } else {
            self.selection_anchor = next;
            self.selection_head = next;
        }
        self.preferred_x = None;
        true
    }

    fn insert_control_text(&mut self, text: &str) -> bool {
        self.replace_selection(text);
        self.preferred_x = None;
        true
    }

    fn backspace(&mut self) -> bool {
        if self.selection_range().is_some() {
            self.replace_selection("");
            self.preferred_x = None;
            self.request_caret_visibility();
            return true;
        }
        if self.selection_head == 0 {
            return false;
        }
        let remove_start = self.selection_head - 1;
        self.replace_char_range(remove_start, self.selection_head, "");
        self.selection_anchor = remove_start;
        self.selection_head = remove_start;
        self.preferred_x = None;
        self.request_caret_visibility();
        true
    }

    fn delete_forward(&mut self) -> bool {
        if self.selection_range().is_some() {
            self.replace_selection("");
            self.preferred_x = None;
            self.request_caret_visibility();
            return true;
        }
        if self.selection_head >= self.char_len() {
            return false;
        }
        let remove_end = self.selection_head + 1;
        self.replace_char_range(self.selection_head, remove_end, "");
        self.preferred_x = None;
        self.request_caret_visibility();
        true
    }

    fn replace_selection(&mut self, replacement: &str) {
        let (start, end) = self
            .selection_range()
            .unwrap_or((self.selection_head, self.selection_head));
        self.replace_char_range(start, end, replacement);
        let next = start + replacement.chars().count();
        self.selection_anchor = next;
        self.selection_head = next;
        self.request_caret_visibility();
    }

    fn replace_char_range(&mut self, start: usize, end: usize, replacement: &str) {
        let start_byte = byte_index_for_char(&self.text, start);
        let end_byte = byte_index_for_char(&self.text, end);
        self.text.replace_range(start_byte..end_byte, replacement);
    }

    fn line_ranges(&self) -> Vec<(usize, usize)> {
        let mut ranges = Vec::new();
        let mut start = 0usize;
        let mut char_index = 0usize;
        for ch in self.text.chars() {
            if ch == '\n' {
                ranges.push((start, char_index));
                start = char_index + 1;
            }
            char_index += 1;
        }
        ranges.push((start, char_index));
        if ranges.is_empty() {
            ranges.push((0, 0));
        }
        ranges
    }

    fn slice_chars(&self, start: usize, end: usize) -> String {
        self.text
            .chars()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect()
    }

    fn char_len(&self) -> usize {
        self.text.chars().count()
    }

    fn hit_test(&self, point: Point) -> usize {
        if self.layout_cache.lines.is_empty() {
            return 0;
        }
        let line_index = self
            .layout_cache
            .lines
            .iter()
            .position(|line| point.y < line.rect.y + line.rect.height)
            .unwrap_or(self.layout_cache.lines.len() - 1);
        let line = &self.layout_cache.lines[line_index];
        let local_x = (point.x - line.rect.x + self.scroll_x).max(0.0);
        line.source_start + closest_char_offset_index(&line.char_offsets, local_x)
    }

    fn line_position_for_caret(&self) -> Option<(usize, usize, f32)> {
        self.layout_cache
            .lines
            .iter()
            .enumerate()
            .find_map(|(index, line)| {
                if self.selection_head >= line.source_start
                    && self.selection_head <= line.source_end
                {
                    let local = self.selection_head - line.source_start;
                    let x = *line.char_offsets.get(local).unwrap_or(&0.0);
                    Some((index, local, x))
                } else {
                    None
                }
            })
    }

    fn clamp_scroll(&mut self) {
        self.horizontal_scrollbar.offset = self.scroll_x;
        self.vertical_scrollbar.offset = self.scroll_y;
        self.horizontal_scrollbar.apply_scroll_delta(0.0);
        self.vertical_scrollbar.apply_scroll_delta(0.0);
        self.sync_scroll_offsets_from_scrollbars();
    }

    fn ensure_caret_visible(&mut self) {
        let Some(caret) = self.layout_cache.caret_rect else {
            return;
        };
        let content = self.layout_cache.content_rect;
        if caret.x < content.x {
            self.scroll_x = (self.scroll_x - (content.x - caret.x)).max(0.0);
        } else if caret.x + caret.width > content.x + content.width {
            self.scroll_x += (caret.x + caret.width) - (content.x + content.width);
        }
        if caret.y < content.y {
            self.scroll_y = (self.scroll_y - (content.y - caret.y)).max(0.0);
        } else if caret.y + caret.height > content.y + content.height {
            self.scroll_y += (caret.y + caret.height) - (content.y + content.height);
        }
    }

    fn scroll_vertical(&mut self, delta: f32) -> bool {
        let next = self.vertical_scrollbar.offset + delta;
        if (next - self.vertical_scrollbar.offset).abs() <= f32::EPSILON {
            return false;
        }
        self.vertical_scrollbar.apply_scroll_delta(delta);
        self.sync_scroll_offsets_from_scrollbars();
        self.caret_visibility_pending = false;
        true
    }

    pub fn scroll_horizontal(&mut self, delta: f32) -> bool {
        let next = self.horizontal_scrollbar.offset + delta;
        if (next - self.horizontal_scrollbar.offset).abs() <= f32::EPSILON {
            return false;
        }
        self.horizontal_scrollbar.apply_scroll_delta(delta);
        self.sync_scroll_offsets_from_scrollbars();
        self.caret_visibility_pending = false;
        true
    }

    fn horizontal_scrollbar_total_height(&self) -> f32 {
        Self::HSCROLL_GAP + Self::HSCROLL_HEIGHT
    }

    fn vertical_scrollbar_total_width(&self) -> f32 {
        Self::VSCROLL_GAP + Self::VSCROLL_WIDTH
    }

    fn layout_scrollbars(
        &mut self,
        base_content_rect: Rect,
        content_width: f32,
        content_height: f32,
    ) -> Rect {
        let mut need_h = content_width > base_content_rect.width + f32::EPSILON;
        let mut need_v = content_height > base_content_rect.height + f32::EPSILON;

        for _ in 0..2 {
            let width = (base_content_rect.width
                - if need_v {
                    self.vertical_scrollbar_total_width()
                } else {
                    0.0
                })
            .max(0.0);
            let height = (base_content_rect.height
                - if need_h {
                    self.horizontal_scrollbar_total_height()
                } else {
                    0.0
                })
            .max(0.0);
            let next_h = content_width > width + f32::EPSILON;
            let next_v = content_height > height + f32::EPSILON;
            if next_h == need_h && next_v == need_v {
                break;
            }
            need_h = next_h;
            need_v = next_v;
        }

        let content_rect = Rect {
            x: base_content_rect.x,
            y: base_content_rect.y,
            width: (base_content_rect.width
                - if need_v {
                    self.vertical_scrollbar_total_width()
                } else {
                    0.0
                })
            .max(0.0),
            height: (base_content_rect.height
                - if need_h {
                    self.horizontal_scrollbar_total_height()
                } else {
                    0.0
                })
            .max(0.0),
        };

        self.horizontal_scrollbar.offset = self.scroll_x;
        self.vertical_scrollbar.offset = self.scroll_y;
        self.horizontal_scrollbar.set_viewport_span(content_rect.width);
        self.horizontal_scrollbar
            .set_content_span(content_width.max(content_rect.width));
        self.vertical_scrollbar.set_viewport_span(content_rect.height);
        self.vertical_scrollbar
            .set_content_span(content_height.max(content_rect.height));
        self.horizontal_scrollbar.set_track_rect(if need_h {
            Rect {
                x: content_rect.x,
                y: content_rect.bottom() + Self::HSCROLL_GAP,
                width: content_rect.width,
                height: Self::HSCROLL_HEIGHT,
            }
        } else {
            Rect::default()
        });
        self.vertical_scrollbar.set_track_rect(if need_v {
            Rect {
                x: content_rect.right() + Self::VSCROLL_GAP,
                y: content_rect.y,
                width: Self::VSCROLL_WIDTH,
                height: content_rect.height,
            }
        } else {
            Rect::default()
        });
        self.sync_scroll_offsets_from_scrollbars();
        content_rect
    }

    fn drag_horizontal_scroll_to(&mut self, pointer_x: f32, drag: ScrollbarDragState) {
        self.horizontal_scrollbar.drag_indicator_to(
            Point {
                x: pointer_x,
                y: self.horizontal_scrollbar.track_rect.y,
            },
            drag,
        );
        self.sync_scroll_offsets_from_scrollbars();
    }

    fn drag_vertical_scroll_to(&mut self, pointer_y: f32, drag: ScrollbarDragState) {
        self.vertical_scrollbar.drag_indicator_to(
            Point {
                x: self.vertical_scrollbar.track_rect.x,
                y: pointer_y,
            },
            drag,
        );
        self.sync_scroll_offsets_from_scrollbars();
    }

    fn sync_scroll_offsets_from_scrollbars(&mut self) {
        self.scroll_x = self.horizontal_scrollbar.offset;
        self.scroll_y = self.vertical_scrollbar.offset;
    }

    fn request_caret_visibility(&mut self) {
        self.caret_visibility_pending = true;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextArea {
    pub id: i32,
    pub model: TextAreaModel,
}

impl TextArea {
    pub fn new(id: i32, text: impl Into<String>, bounds: Rect) -> Self {
        Self {
            id,
            model: TextAreaModel::with_id(WidgetId(id as u64), text, bounds),
        }
    }
}

impl Component for TextArea {
    fn bounds(&self) -> Rect {
        self.model.bounds
    }

    fn set_bounds(&mut self, rect: Rect) {
        self.model.set_bounds(rect);
    }

    fn focus_changed(&mut self, gained: bool) {
        let _ = self.model.handle_event(UiEvent::FocusChanged(gained));
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

fn build_display_text<F>(
    source_text: &str,
    tab_spaces: usize,
    font_size: u16,
    measure_width: &mut F,
) -> (String, Vec<f32>)
where
    F: FnMut(&str, u16) -> f32,
{
    let mut display_text = String::new();
    let mut char_offsets = Vec::with_capacity(source_text.chars().count() + 1);
    char_offsets.push(0.0);
    for ch in source_text.chars() {
        let segment = if ch == '\t' {
            " ".repeat(tab_spaces.max(1))
        } else {
            ch.to_string()
        };
        display_text.push_str(&segment);
        char_offsets.push(measure_width(&display_text, font_size));
    }
    (display_text, char_offsets)
}

fn byte_index_for_char(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(byte_index, _)| byte_index)
        .unwrap_or(text.len())
}

fn ordered_range(a: usize, b: usize) -> (usize, usize) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

fn closest_char_offset_index(offsets: &[f32], target_x: f32) -> usize {
    let mut best_index = 0usize;
    let mut best_distance = f32::MAX;
    for (index, offset) in offsets.iter().enumerate() {
        let distance = (offset - target_x).abs();
        if distance < best_distance {
            best_distance = distance;
            best_index = index;
        }
    }
    best_index
}

fn rect_intersects(a: Rect, b: Rect) -> bool {
    a.x < b.x + b.width && a.x + a.width > b.x && a.y < b.y + b.height && a.y + a.height > b.y
}

#[cfg(test)]
mod tests {
    use super::TextAreaModel;
    use crate::{
        geometry::{Point, Rect},
        input::{Key, Modifiers, PointerButton, PointerState, UiEvent},
    };

    fn measure_width(text: &str, font_size: u16) -> f32 {
        text.chars().count() as f32 * (font_size as f32 * 0.5)
    }

    fn area(text: &str) -> TextAreaModel {
        let mut area = TextAreaModel::new(
            text,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 320.0,
                height: 240.0,
            },
        );
        area.focused = true;
        area.relayout(measure_width);
        area
    }

    #[test]
    fn text_input_inserts_and_backspace_removes() {
        let mut area = area("abc");
        area.selection_anchor = 3;
        area.selection_head = 3;

        let _ = area.handle_event(UiEvent::TextInput {
            text: "d".to_string(),
        });
        assert_eq!(area.text, "abcd");

        let _ = area.handle_event(UiEvent::KeyPressed {
            key: Key::Backspace,
            modifiers: Modifiers::default(),
        });
        assert_eq!(area.text, "abc");
    }

    #[test]
    fn selection_replacement_overwrites_source_range() {
        let mut area = area("alpha beta");
        area.selection_anchor = 6;
        area.selection_head = 10;
        let _ = area.handle_event(UiEvent::TextInput {
            text: "gamma".to_string(),
        });

        assert_eq!(area.text, "alpha gamma");
        assert_eq!(area.selection_anchor, 11);
        assert_eq!(area.selection_head, 11);
    }

    #[test]
    fn vertical_motion_tracks_visual_line_column() {
        let mut area = area("alpha\nbeta\ngamma");
        area.selection_anchor = 2;
        area.selection_head = 2;
        area.relayout(measure_width);

        let _ = area.handle_event(UiEvent::KeyPressed {
            key: Key::Down,
            modifiers: Modifiers::default(),
        });
        assert_eq!(area.selection_head, 8);

        area.relayout(measure_width);
        let _ = area.handle_event(UiEvent::KeyPressed {
            key: Key::Down,
            modifiers: Modifiers::default(),
        });
        assert_eq!(area.selection_head, 13);
    }

    #[test]
    fn trailing_newline_preserves_empty_final_line() {
        let mut area = area("one\n");
        area.relayout(measure_width);
        assert_eq!(area.layout_cache.lines.len(), 2);
        assert_eq!(area.layout_cache.lines[1].source_start, 4);
        assert_eq!(area.layout_cache.lines[1].source_end, 4);
    }

    #[test]
    fn pointer_press_and_drag_updates_selection() {
        let mut area = area("abcd\nefgh");
        area.relayout(measure_width);
        let down = PointerState::mouse(Point { x: 20.0, y: 12.0 }, Modifiers::default());
        let moved = PointerState::mouse(Point { x: 40.0, y: 12.0 }, Modifiers::default());

        let _ = area.handle_event(UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state: down,
        });
        let _ = area.handle_event(UiEvent::PointerMoved(moved));

        assert!(area.selection_range().is_some());
    }

    #[test]
    fn ctrl_a_selects_all_text() {
        let mut area = area("hello");
        let _ = area.handle_event(UiEvent::KeyPressed {
            key: Key::Character('a'),
            modifiers: Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
        });

        assert_eq!(area.selection_range(), Some((0, 5)));
    }

    #[test]
    fn caret_offsets_follow_prefix_measurement_not_per_character_sums() {
        fn kerned_measure(text: &str, _font_size: u16) -> f32 {
            match text {
                "A" => 10.0,
                "V" => 10.0,
                "AV" => 17.0,
                _ => text.chars().count() as f32 * 10.0,
            }
        }

        let mut area = TextAreaModel::new(
            "AV",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 80.0,
            },
        );
        area.focused = true;
        area.relayout(kerned_measure);

        let line = &area.layout_cache.lines[0];
        assert_eq!(line.char_offsets, vec![0.0, 10.0, 17.0]);
    }

    #[test]
    fn relayout_scrolls_horizontally_to_keep_caret_visible() {
        let mut area = TextAreaModel::new(
            "this is a deliberately long line for horizontal scrolling",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 80.0,
            },
        );
        area.focused = true;
        let len = area.text.chars().count();
        area.selection_anchor = len;
        area.selection_head = len;

        area.relayout(measure_width);

        assert!(area.scroll_x > 0.0);
        let caret = area.layout_cache.caret_rect.expect("caret rect");
        let content = area.layout_cache.content_rect;
        assert!(caret.x + caret.width <= content.x + content.width + 0.01);
    }

    #[test]
    fn paint_clips_scrolled_text_to_content_rect() {
        let mut area = TextAreaModel::new(
            "this is a deliberately long line for horizontal scrolling",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 80.0,
            },
        );
        area.focused = true;
        let len = area.text.chars().count();
        area.selection_anchor = len;
        area.selection_head = len;
        area.relayout(measure_width);

        let content = area.layout_cache.content_rect;
        let mut ops = Vec::new();
        area.paint(&mut ops);
        let text_op = ops
            .into_iter()
            .find_map(|op| match op {
                crate::paint::PaintOp::Text {
                    rect, clip_rect, ..
                } => Some((rect, clip_rect)),
                _ => None,
            })
            .expect("text area paint should emit text");

        assert!(text_op.0.x < content.x);
        assert_eq!(text_op.1, Some(content));
    }

    #[test]
    fn horizontal_scrollbar_paints_when_content_exceeds_width() {
        let mut area = TextAreaModel::new(
            "this is a deliberately long line for horizontal scrolling",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 80.0,
            },
        );
        area.focused = true;
        let len = area.text.chars().count();
        area.selection_anchor = len;
        area.selection_head = len;
        area.relayout(measure_width);

        let mut ops = Vec::new();
        area.paint(&mut ops);
        let fill_rect_count = ops
            .into_iter()
            .filter(|op| matches!(op, crate::paint::PaintOp::FillRect { .. }))
            .count();
        assert!(fill_rect_count >= 3);
        assert!(area.horizontal_scrollbar.is_scrollable());
        assert!(area.horizontal_scrollbar.indicator_thumb_rect().is_some());
    }

    #[test]
    fn dragging_horizontal_scrollbar_updates_scroll_x() {
        let mut area = TextAreaModel::new(
            "this is a deliberately long line for horizontal scrolling",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 80.0,
            },
        );
        area.focused = true;
        area.relayout(measure_width);
        let thumb = area
            .horizontal_scrollbar
            .indicator_thumb_rect()
            .expect("horizontal thumb");
        let pointer = Point {
            x: thumb.x + 4.0,
            y: thumb.y + thumb.height * 0.5,
        };
        let _ = area.handle_event(UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state: PointerState::mouse(pointer, Modifiers::default()),
        });
        let moved_pointer = Point {
            x: pointer.x + 24.0,
            y: pointer.y,
        };
        let _ = area.handle_event(UiEvent::PointerMoved(PointerState::mouse(
            moved_pointer,
            Modifiers::default(),
        )));

        assert!(area.scroll_x > 0.0);
        assert!(area.horizontal_drag.is_some());
    }

    #[test]
    fn prefers_text_cursor_excludes_horizontal_scrollbar_track() {
        let mut area = TextAreaModel::new(
            "this is a deliberately long line for horizontal scrolling",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 80.0,
            },
        );
        area.focused = true;
        area.relayout(measure_width);

        let text_point = Point {
            x: area.layout_cache.content_rect.x + 8.0,
            y: area.layout_cache.content_rect.y + 8.0,
        };
        assert!(area.prefers_text_cursor(text_point));

        let track = area
            .horizontal_scrollbar
            .interactive_rect(
                TextAreaModel::HSCROLL_HIT_PAD_Y,
                TextAreaModel::HSCROLL_HIT_PAD_X,
            )
            .expect("horizontal scrollbar track");
        let track_point = Point {
            x: track.x + 4.0,
            y: track.y + track.height * 0.5,
        };
        assert!(!area.prefers_text_cursor(track_point));
    }

    #[test]
    fn clicking_scrollbar_hit_area_does_not_move_text_caret() {
        let mut area = TextAreaModel::new(
            "this is a deliberately long line for horizontal scrolling",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 80.0,
            },
        );
        area.focused = true;
        let len = area.text.chars().count();
        area.selection_anchor = len;
        area.selection_head = len;
        area.relayout(measure_width);
        let caret_before = area.selection_head;
        let hit = area
            .horizontal_scrollbar
            .interactive_rect(
                TextAreaModel::HSCROLL_HIT_PAD_Y,
                TextAreaModel::HSCROLL_HIT_PAD_X,
            )
            .expect("hit rect");
        let pointer = Point {
            x: hit.right() - 4.0,
            y: hit.y + hit.height * 0.5,
        };
        let _ = area.handle_event(UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state: PointerState::mouse(pointer, Modifiers::default()),
        });
        assert_eq!(area.selection_head, caret_before);
        assert!(!area.drag_selecting);
    }

    #[test]
    fn manual_horizontal_scroll_can_leave_caret_offscreen() {
        let mut area = TextAreaModel::new(
            "this is a deliberately long line for horizontal scrolling",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 80.0,
            },
        );
        area.focused = true;
        let len = area.text.chars().count();
        area.selection_anchor = len;
        area.selection_head = len;
        area.relayout(measure_width);

        let scroll_before = area.scroll_x;
        let moved = area.scroll_horizontal(-48.0);
        assert!(moved);
        area.relayout(measure_width);

        assert!(area.scroll_x < scroll_before);
        let caret = area.layout_cache.caret_rect.expect("caret rect");
        let content = area.layout_cache.content_rect;
        assert!(caret.x + caret.width > content.x + content.width);
    }

    #[test]
    fn key_navigation_requests_caret_visibility_after_manual_scroll() {
        let mut area = TextAreaModel::new(
            "this is a deliberately long line for horizontal scrolling",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 80.0,
            },
        );
        area.focused = true;
        let len = area.text.chars().count();
        area.selection_anchor = len;
        area.selection_head = len;
        area.relayout(measure_width);

        let _ = area.scroll_horizontal(-48.0);
        area.relayout(measure_width);
        let content = area.layout_cache.content_rect;
        let caret_before = area.layout_cache.caret_rect.expect("caret rect before");
        assert!(caret_before.x + caret_before.width > content.x + content.width);

        let _ = area.handle_event(UiEvent::KeyPressed {
            key: Key::Left,
            modifiers: Modifiers::default(),
        });
        area.relayout(measure_width);

        let caret_after = area.layout_cache.caret_rect.expect("caret rect after");
        assert!(caret_after.x + caret_after.width <= content.x + content.width + 0.01);
    }
}
