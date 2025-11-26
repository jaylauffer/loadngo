use std::collections::HashMap;

use crate::{
    component::Component,
    geometry::{Color, Insets, Point, Rect},
    input::{Key, PointerButton, UiEvent},
    paint::{HorizontalAlign, PaintOp, TextLayoutMode, TextOverflow, TextStyle, VerticalAlign},
    scroll::{ScrollbarAxis, ScrollbarDragState, ScrollbarModel},
    text::single_line_text_box_height,
    text_document::TextDocument,
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
struct CachedLineMetrics {
    display_text: String,
    char_offsets: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
struct DocumentLineMetrics {
    source_start: usize,
    source_end: usize,
    display_text: String,
    char_offsets: Vec<f32>,
    width: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingTextEdit {
    dirty_line: usize,
    old_suffix_start: usize,
    new_end_line: usize,
    char_delta: isize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextAreaModel {
    pub widget_id: WidgetId,
    pub bounds: Rect,
    pub document: TextDocument,
    pub style: TextStyle,
    pub padding: Insets,
    pub background: Option<Color>,
    pub border: Option<Color>,
    pub selection_fill: Color,
    pub caret_color: Color,
    pub line_spacing: f32,
    pub show_line_numbers: bool,
    pub line_number_color: Color,
    pub line_number_gutter_fill: Option<Color>,
    pub line_number_gutter_border: Option<Color>,
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
    line_starts: Vec<usize>,
    line_index_dirty: bool,
    document_lines: Vec<DocumentLineMetrics>,
    document_dirty_from_line: Option<usize>,
    pending_edit: Option<PendingTextEdit>,
    document_content_width: f32,
    line_metrics_cache: HashMap<String, CachedLineMetrics>,
    cache_font_size: u16,
    cache_tab_spaces: usize,
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
        let text = text.into();
        let mut style = TextStyle::default();
        style.layout_mode = TextLayoutMode::MultiLine;
        style.horizontal_align = HorizontalAlign::Left;
        style.vertical_align = VerticalAlign::Top;
        style.vertical_metric_mode = crate::TextVerticalMetricMode::LogicalLineBox;
        style.overflow = TextOverflow::Clip;
        style.color = Color::rgba(0xeb, 0xef, 0xf7, 0xff);
        let cache_font_size = style.font_size;
        Self {
            widget_id: WidgetId(0),
            bounds,
            document: TextDocument::new(text),
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
            show_line_numbers: false,
            line_number_color: Color::rgba(0x8d, 0x9a, 0xae, 0xff),
            line_number_gutter_fill: Some(Color::rgba(0x10, 0x15, 0x1d, 0xf6)),
            line_number_gutter_border: Some(Color::rgba(0x36, 0x41, 0x53, 0xff)),
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
            vertical_scrollbar: ScrollbarModel::new(ScrollbarAxis::Vertical, Rect::default(), 0.0),
            line_starts: vec![0],
            line_index_dirty: true,
            document_lines: Vec::new(),
            document_dirty_from_line: Some(0),
            pending_edit: None,
            document_content_width: 0.0,
            line_metrics_cache: HashMap::new(),
            cache_font_size,
            cache_tab_spaces: 4,
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
        let base = self.text_base_content_rect();
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

    fn line_number_digits(&self) -> usize {
        self.line_starts.len().max(1).to_string().len().max(2)
    }

    fn line_number_gutter_width(&self) -> f32 {
        if !self.show_line_numbers {
            return 0.0;
        }
        let digits = self.line_number_digits() as f32;
        16.0 + digits * (self.style.font_size as f32 * 0.62)
    }

    fn gutter_rect(&self) -> Option<Rect> {
        if !self.show_line_numbers {
            return None;
        }
        let base = self.base_content_rect();
        Some(Rect {
            x: base.x,
            y: base.y,
            width: self.line_number_gutter_width().min(base.width),
            height: base.height,
        })
    }

    fn text_base_content_rect(&self) -> Rect {
        let base = self.base_content_rect();
        let gutter = self.line_number_gutter_width().min(base.width);
        Rect {
            x: base.x + gutter,
            y: base.y,
            width: (base.width - gutter).max(0.0),
            height: base.height,
        }
    }

    pub fn caret(&self) -> usize {
        self.selection_head
    }

    pub fn text(&self) -> String {
        self.document.to_string()
    }

    pub fn revision(&self) -> u64 {
        self.document.revision()
    }

    pub fn prefers_text_cursor(&self, point: Point) -> bool {
        self.layout_cache.content_rect.contains(point)
            && self
                .gutter_rect()
                .is_none_or(|gutter| !gutter.contains(point))
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

    pub fn set_caret(&mut self, caret: usize) {
        let next = caret.min(self.char_len());
        self.selection_anchor = next;
        self.selection_head = next;
        self.preferred_x = None;
        self.request_caret_visibility();
    }

    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.replace_selection(text);
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
        self.ensure_line_index();
        if self.cache_font_size != self.style.font_size || self.cache_tab_spaces != self.tab_spaces
        {
            self.line_metrics_cache.clear();
            self.cache_font_size = self.style.font_size;
            self.cache_tab_spaces = self.tab_spaces;
            self.document_dirty_from_line = Some(0);
            self.pending_edit = None;
        }
        self.ensure_document_layout(measure_width);
        let base_content_rect = self.text_base_content_rect();
        let line_box_height = single_line_text_box_height(self.style.font_size);
        let line_step = line_box_height + self.line_spacing.max(0.0);
        let total_line_count = self.document_lines.len().max(1);
        let mut selection_rects = Vec::new();
        let selection_range = self.selection_range();
        let mut caret_rect = None;
        let content_height = if total_line_count == 0 {
            line_box_height
        } else {
            ((total_line_count - 1) as f32 * line_step + line_box_height).max(line_box_height)
        };
        let content_rect = self.layout_scrollbars(
            base_content_rect,
            self.document_content_width,
            content_height,
        );
        let visible_start = (self.scroll_y / line_step).floor().max(0.0) as usize;
        let visible_end = ((self.scroll_y + content_rect.height) / line_step)
            .ceil()
            .max(0.0) as usize
            + 1;
        let mut lines = Vec::new();
        for index in visible_start.min(total_line_count)..visible_end.min(total_line_count) {
            let metrics = &self.document_lines[index];
            let rect = Rect {
                x: base_content_rect.x,
                y: base_content_rect.y + index as f32 * line_step - self.scroll_y,
                width: content_rect.width,
                height: line_box_height,
            };
            let line = TextAreaLineLayout {
                source_start: metrics.source_start,
                source_end: metrics.source_end,
                display_text: metrics.display_text.clone(),
                rect,
                char_offsets: metrics.char_offsets.clone(),
            };

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

            lines.push(line);
        }

        if let Some((line_index, local, _)) = self.line_position_for_caret() {
            if let Some(metrics) = self.document_lines.get(line_index) {
                let rect_y = base_content_rect.y + line_index as f32 * line_step - self.scroll_y;
                let caret_top = rect_y + 1.0;
                let caret_height =
                    (self.style.font_size as f32 + 1.0).min((line_box_height - 2.0).max(1.0));
                let local = local.min(metrics.char_offsets.len().saturating_sub(1));
                caret_rect = Some(Rect {
                    x: (base_content_rect.x + metrics.char_offsets[local] - self.scroll_x).round(),
                    y: caret_top,
                    width: 1.5,
                    height: caret_height,
                });
            }
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

        self.layout_cache = TextAreaLayoutCache {
            lines,
            selection_rects,
            caret_rect,
            content_rect,
            content_width: self.document_content_width,
            content_height,
        };
        self.clamp_scroll();
    }

    fn cached_display_text<F>(
        &mut self,
        source_text: &str,
        measure_width: &mut F,
    ) -> (String, Vec<f32>)
    where
        F: FnMut(&str, u16) -> f32,
    {
        if let Some(cached) = self.line_metrics_cache.get(source_text) {
            return (cached.display_text.clone(), cached.char_offsets.clone());
        }

        let (display_text, char_offsets) = build_display_text(
            source_text,
            self.tab_spaces,
            self.style.font_size,
            measure_width,
        );
        self.line_metrics_cache.insert(
            source_text.to_string(),
            CachedLineMetrics {
                display_text: display_text.clone(),
                char_offsets: char_offsets.clone(),
            },
        );
        (display_text, char_offsets)
    }

    fn ensure_document_layout<F>(&mut self, measure_width: &mut F)
    where
        F: FnMut(&str, u16) -> f32,
    {
        let Some(dirty_from_line) = self.document_dirty_from_line else {
            return;
        };
        let line_ranges = self.line_ranges();
        let pending_edit = self.pending_edit.take();
        let mut document_lines = if dirty_from_line == 0 {
            Vec::with_capacity(line_ranges.len())
        } else {
            self.document_lines[..dirty_from_line.min(self.document_lines.len())].to_vec()
        };
        let mut content_width = document_lines
            .iter()
            .map(|line| line.width)
            .fold(0.0f32, f32::max);
        let (recompute_end_exclusive, reuse_suffix_start) =
            if let Some(edit) = pending_edit.filter(|edit| edit.dirty_line == dirty_from_line) {
                (
                    (edit.new_end_line + 1).min(line_ranges.len()),
                    Some((edit.old_suffix_start, edit.char_delta)),
                )
            } else {
                (line_ranges.len(), None)
            };
        for (source_start, source_end) in line_ranges
            .iter()
            .copied()
            .skip(dirty_from_line)
            .take(recompute_end_exclusive.saturating_sub(dirty_from_line))
        {
            let source_text = self.slice_chars(source_start, source_end);
            let (display_text, char_offsets) =
                self.cached_display_text(&source_text, measure_width);
            let width = char_offsets.last().copied().unwrap_or(0.0);
            content_width = content_width.max(width);
            document_lines.push(DocumentLineMetrics {
                source_start,
                source_end,
                display_text,
                char_offsets,
                width,
            });
        }
        if let Some((old_suffix_start, char_delta)) = reuse_suffix_start {
            for line in self.document_lines.iter().skip(old_suffix_start) {
                let shifted = DocumentLineMetrics {
                    source_start: ((line.source_start as isize) + char_delta).max(0) as usize,
                    source_end: ((line.source_end as isize) + char_delta).max(0) as usize,
                    display_text: line.display_text.clone(),
                    char_offsets: line.char_offsets.clone(),
                    width: line.width,
                };
                content_width = content_width.max(shifted.width);
                document_lines.push(shifted);
            }
        }
        if document_lines.is_empty() {
            document_lines.push(DocumentLineMetrics {
                source_start: 0,
                source_end: 0,
                display_text: String::new(),
                char_offsets: vec![0.0],
                width: 0.0,
            });
        }
        self.document_lines = document_lines;
        self.document_content_width = content_width;
        self.document_dirty_from_line = None;
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
                        if let Some(drag) =
                            self.vertical_scrollbar.begin_indicator_drag(state.position)
                        {
                            self.vertical_drag = Some(drag);
                            self.vertical_scrollbar
                                .drag_indicator_to(state.position, drag);
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
                        if let Some(drag) = self
                            .horizontal_scrollbar
                            .begin_indicator_drag(state.position)
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
        if let Some(gutter) = self.gutter_rect() {
            if let Some(color) = self.line_number_gutter_fill {
                scene.push(PaintOp::FillRect {
                    rect: gutter,
                    color,
                });
            }
            if let Some(color) = self.line_number_gutter_border {
                scene.push(PaintOp::Line {
                    from: Point {
                        x: gutter.x + gutter.width,
                        y: gutter.y,
                    },
                    to: Point {
                        x: gutter.x + gutter.width,
                        y: gutter.y + gutter.height,
                    },
                    color,
                });
            }
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
            if let Some(gutter) = self.gutter_rect() {
                let line_number = self
                    .line_starts
                    .partition_point(|&line_start| line_start <= line.source_start)
                    .max(1);
                scene.push(PaintOp::Text {
                    rect: Rect {
                        x: gutter.x + 4.0,
                        y: line.rect.y,
                        width: (gutter.width - 8.0).max(0.0),
                        height: line.rect.height,
                    },
                    clip_rect: Some(gutter),
                    text: line_number.to_string(),
                    style: TextStyle {
                        color: self.line_number_color,
                        font_size: self.style.font_size.saturating_sub(1).max(10),
                        horizontal_align: HorizontalAlign::Right,
                        vertical_align: VerticalAlign::Top,
                        layout_mode: TextLayoutMode::SingleLine,
                        overflow: TextOverflow::Clip,
                        ..self.style.clone()
                    },
                });
            }
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
        if matches!(key, Key::Character('z') | Key::Character('Z'))
            && (modifiers.ctrl || modifiers.meta)
        {
            let changed = if modifiers.shift {
                self.redo()
            } else {
                self.undo()
            };
            return if changed {
                WidgetResponse::redraw_consumed()
            } else {
                WidgetResponse::default()
            };
        }
        if matches!(key, Key::Character('y') | Key::Character('Y'))
            && (modifiers.ctrl || modifiers.meta)
        {
            return if self.redo() {
                WidgetResponse::redraw_consumed()
            } else {
                WidgetResponse::default()
            };
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
            .clamp(0, self.document_lines.len().saturating_sub(1) as isize)
            as usize;
        if next_line_index == line_index && direction != 0 {
            return false;
        }
        let preferred_x = self.preferred_x.unwrap_or(caret_x);
        let next_line = &self.document_lines[next_line_index];
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
        let line = &self.document_lines[line_index];
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
        self.ensure_line_index();
        let dirty_line = self.line_index_for_char(start);
        let old_suffix_start = self
            .line_starts
            .partition_point(|&line_start| line_start <= end);
        let new_end_line = dirty_line + replacement.chars().filter(|&ch| ch == '\n').count();
        let char_delta = replacement.chars().count() as isize - end.saturating_sub(start) as isize;
        self.update_line_starts_for_replace(start, end, replacement);
        self.document.replace_char_range(start, end, replacement);
        self.pending_edit = Some(PendingTextEdit {
            dirty_line,
            old_suffix_start,
            new_end_line,
            char_delta,
        });
        self.document_dirty_from_line = Some(
            self.document_dirty_from_line
                .map(|existing| existing.min(dirty_line))
                .unwrap_or(dirty_line),
        );
    }

    fn line_ranges(&self) -> Vec<(usize, usize)> {
        let char_len = self.char_len();
        let mut ranges = Vec::with_capacity(self.line_starts.len());
        for (index, start) in self.line_starts.iter().copied().enumerate() {
            let end = self
                .line_starts
                .get(index + 1)
                .map(|next| next.saturating_sub(1))
                .unwrap_or(char_len);
            ranges.push((start, end));
        }
        ranges
    }

    fn slice_chars(&self, start: usize, end: usize) -> String {
        self.document.slice_chars(start, end)
    }

    fn char_len(&self) -> usize {
        self.document.len_chars()
    }

    fn hit_test(&self, point: Point) -> usize {
        if self.document_lines.is_empty() {
            return 0;
        }
        let line_step =
            single_line_text_box_height(self.style.font_size) + self.line_spacing.max(0.0);
        let local_y = (point.y - self.text_base_content_rect().y + self.scroll_y).max(0.0);
        let line_index = (local_y / line_step)
            .floor()
            .clamp(0.0, self.document_lines.len().saturating_sub(1) as f32)
            as usize;
        let line = &self.document_lines[line_index];
        let local_x = (point.x - self.text_base_content_rect().x + self.scroll_x).max(0.0);
        line.source_start + closest_char_offset_index(&line.char_offsets, local_x)
    }

    fn line_position_for_caret(&self) -> Option<(usize, usize, f32)> {
        self.document_lines
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
            self.scroll_x = (self.scroll_x - (content.x - caret.x).ceil()).max(0.0);
        } else if caret.x + caret.width > content.x + content.width {
            self.scroll_x += ((caret.x + caret.width) - (content.x + content.width)).ceil();
        }
        if caret.y < content.y {
            self.scroll_y = (self.scroll_y - (content.y - caret.y).ceil()).max(0.0);
        } else if caret.y + caret.height > content.y + content.height {
            self.scroll_y += ((caret.y + caret.height) - (content.y + content.height)).ceil();
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
        self.horizontal_scrollbar
            .set_viewport_span(content_rect.width);
        self.horizontal_scrollbar
            .set_content_span(content_width.max(content_rect.width));
        self.vertical_scrollbar
            .set_viewport_span(content_rect.height);
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

    fn line_index_for_char(&self, char_index: usize) -> usize {
        self.line_starts
            .partition_point(|&start| start <= char_index)
            .saturating_sub(1)
    }

    fn ensure_line_index(&mut self) {
        if !self.line_index_dirty {
            return;
        }
        self.line_starts.clear();
        self.line_starts.push(0);
        let mut char_index = 0usize;
        self.document.for_each_chunk(|chunk| {
            for ch in chunk.chars() {
                char_index += 1;
                if ch == '\n' {
                    self.line_starts.push(char_index);
                }
            }
        });
        if self.line_starts.is_empty() {
            self.line_starts.push(0);
        }
        self.line_index_dirty = false;
    }

    fn update_line_starts_for_replace(&mut self, start: usize, end: usize, replacement: &str) {
        let start = start.min(self.char_len());
        let end = end.min(self.char_len());
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let prefix_len = self
            .line_starts
            .partition_point(|&line_start| line_start <= start);
        let suffix_start = self
            .line_starts
            .partition_point(|&line_start| line_start <= end);
        let inserted_chars = replacement.chars().count();
        let removed_chars = end.saturating_sub(start);
        let delta = inserted_chars as isize - removed_chars as isize;

        let mut updated = Vec::with_capacity(
            prefix_len
                + replacement.chars().filter(|&ch| ch == '\n').count()
                + self.line_starts.len().saturating_sub(suffix_start),
        );
        updated.extend_from_slice(&self.line_starts[..prefix_len]);

        let mut local_char_index = 0usize;
        for ch in replacement.chars() {
            local_char_index += 1;
            if ch == '\n' {
                updated.push(start + local_char_index);
            }
        }

        for &line_start in &self.line_starts[suffix_start..] {
            updated.push(((line_start as isize) + delta).max(0) as usize);
        }

        if updated.is_empty() || updated[0] != 0 {
            updated.insert(0, 0);
        }
        self.line_starts = updated;
        self.line_index_dirty = false;
    }

    pub fn undo(&mut self) -> bool {
        if !self.document.undo() {
            return false;
        }
        self.after_document_history_change();
        true
    }

    pub fn redo(&mut self) -> bool {
        if !self.document.redo() {
            return false;
        }
        self.after_document_history_change();
        true
    }

    fn after_document_history_change(&mut self) {
        self.line_index_dirty = true;
        self.document_dirty_from_line = Some(0);
        self.pending_edit = None;
        let len = self.char_len();
        self.selection_anchor = self.selection_anchor.min(len);
        self.selection_head = self.selection_head.min(len);
        self.preferred_x = None;
        self.request_caret_visibility();
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
        assert_eq!(area.text(), "abcd");

        let _ = area.handle_event(UiEvent::KeyPressed {
            key: Key::Backspace,
            modifiers: Modifiers::default(),
        });
        assert_eq!(area.text(), "abc");
    }

    #[test]
    fn command_z_and_redo_restore_document_edits() {
        let mut area = area("abc");
        area.selection_anchor = 3;
        area.selection_head = 3;
        let _ = area.handle_event(UiEvent::TextInput {
            text: "d".to_string(),
        });
        assert_eq!(area.text(), "abcd");

        let _ = area.handle_event(UiEvent::KeyPressed {
            key: Key::Character('z'),
            modifiers: Modifiers {
                meta: true,
                ..Modifiers::default()
            },
        });
        assert_eq!(area.text(), "abc");

        let _ = area.handle_event(UiEvent::KeyPressed {
            key: Key::Character('z'),
            modifiers: Modifiers {
                meta: true,
                shift: true,
                ..Modifiers::default()
            },
        });
        assert_eq!(area.text(), "abcd");
    }

    #[test]
    fn selection_replacement_overwrites_source_range() {
        let mut area = area("alpha beta");
        area.selection_anchor = 6;
        area.selection_head = 10;
        let _ = area.handle_event(UiEvent::TextInput {
            text: "gamma".to_string(),
        });

        assert_eq!(area.text(), "alpha gamma");
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
    fn up_arrow_moves_caret_to_previous_line() {
        let mut area = area("alpha\nbeta\ngamma");
        area.selection_anchor = 8;
        area.selection_head = 8;
        area.relayout(measure_width);

        let _ = area.handle_event(UiEvent::KeyPressed {
            key: Key::Up,
            modifiers: Modifiers::default(),
        });
        assert_eq!(area.selection_head, 2);
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
        let len = area.text().chars().count();
        area.selection_anchor = len;
        area.selection_head = len;

        area.relayout(measure_width);

        assert!(area.scroll_x > 0.0);
        let caret = area.layout_cache.caret_rect.expect("caret rect");
        let content = area.layout_cache.content_rect;
        assert!(caret.x <= content.x + content.width + 0.01);
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
        let len = area.text().chars().count();
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
        let len = area.text().chars().count();
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
        let len = area.text().chars().count();
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
        let len = area.text().chars().count();
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
        let len = area.text().chars().count();
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

    #[test]
    fn relayout_reuses_cached_line_metrics_for_unchanged_lines() {
        let mut area = TextAreaModel::new(
            "alpha\nbeta\ngamma",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 320.0,
                height: 240.0,
            },
        );
        area.focused = true;

        let mut first_calls = 0usize;
        area.relayout(|text, font_size| {
            first_calls += 1;
            measure_width(text, font_size)
        });
        assert!(first_calls > 0);

        let mut second_calls = 0usize;
        area.relayout(|text, font_size| {
            second_calls += 1;
            measure_width(text, font_size)
        });
        assert_eq!(second_calls, 0);
    }

    #[test]
    fn single_line_edit_remeasures_less_than_initial_full_layout() {
        let mut area = TextAreaModel::new(
            "alpha\nbeta\ngamma\ndelta",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 320.0,
                height: 240.0,
            },
        );
        area.focused = true;

        let mut first_calls = 0usize;
        area.relayout(|text, font_size| {
            first_calls += 1;
            measure_width(text, font_size)
        });

        area.selection_anchor = 2;
        area.selection_head = 2;
        let _ = area.handle_event(UiEvent::TextInput {
            text: "Z".to_string(),
        });

        let mut second_calls = 0usize;
        area.relayout(|text, font_size| {
            second_calls += 1;
            measure_width(text, font_size)
        });

        assert!(second_calls > 0);
        assert!(second_calls < first_calls);
    }

    #[test]
    fn localized_edit_reuses_unchanged_suffix_document_lines() {
        let mut area = TextAreaModel::new(
            "alpha\nbeta\ngamma\ndelta",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 320.0,
                height: 240.0,
            },
        );
        area.focused = true;

        let mut first_calls = 0usize;
        area.relayout(|text, font_size| {
            first_calls += 1;
            measure_width(text, font_size)
        });
        area.selection_anchor = 1;
        area.selection_head = 1;
        let _ = area.handle_event(UiEvent::TextInput {
            text: "Z".to_string(),
        });

        let mut second_calls = 0usize;
        area.relayout(|text, font_size| {
            second_calls += 1;
            measure_width(text, font_size)
        });

        assert!(second_calls < first_calls);
        assert_eq!(area.document_lines[1].display_text, "beta");
        assert_eq!(area.document_lines[2].display_text, "gamma");
        assert_eq!(area.document_lines[3].display_text, "delta");
    }

    #[test]
    fn replace_updates_line_starts_without_marking_index_dirty() {
        let mut area = area("alpha\nbeta\ngamma");
        area.selection_anchor = 5;
        area.selection_head = 5;
        let _ = area.handle_event(UiEvent::TextInput {
            text: "\n".to_string(),
        });

        assert_eq!(area.line_starts, vec![0, 6, 7, 12]);
        assert!(!area.line_index_dirty);
    }

    #[test]
    fn deleting_newline_merges_line_starts_incrementally() {
        let mut area = area("alpha\nbeta\ngamma");
        area.selection_anchor = 5;
        area.selection_head = 6;
        let _ = area.handle_event(UiEvent::KeyPressed {
            key: Key::Backspace,
            modifiers: Modifiers::default(),
        });

        assert_eq!(area.line_starts, vec![0, 10]);
        assert!(!area.line_index_dirty);
    }

    #[test]
    fn line_number_gutter_shifts_text_viewport_and_paints_numbers() {
        let mut area = area("alpha\nbeta\ngamma");
        area.show_line_numbers = true;
        area.relayout(measure_width);

        let gutter = area.gutter_rect().expect("gutter");
        let content = area.layout_cache.content_rect;
        assert!(content.x >= gutter.right());

        let mut ops = Vec::new();
        area.paint(&mut ops);
        let line_number_texts: Vec<String> = ops
            .into_iter()
            .filter_map(|op| match op {
                crate::paint::PaintOp::Text { text, rect, .. } if rect.x < content.x => Some(text),
                _ => None,
            })
            .collect();
        assert!(line_number_texts.iter().any(|text| text == "1"));
        assert!(line_number_texts.iter().any(|text| text == "2"));
    }

    #[test]
    fn backspacing_line_contents_to_empty_preserves_line_numbers_and_vertical_motion() {
        let mut area = area("one\ntwo\nthree\nfour");
        area.show_line_numbers = true;
        area.relayout(measure_width);

        area.selection_anchor = 7;
        area.selection_head = 7;
        for _ in 0..3 {
            let _ = area.handle_event(UiEvent::KeyPressed {
                key: Key::Backspace,
                modifiers: Modifiers::default(),
            });
        }
        area.relayout(measure_width);

        assert_eq!(area.text(), "one\n\nthree\nfour");
        assert_eq!(area.line_starts, vec![0, 4, 5, 11]);

        let painted_numbers: Vec<String> = {
            let mut ops = Vec::new();
            area.paint(&mut ops);
            ops.into_iter()
                .filter_map(|op| match op {
                    crate::paint::PaintOp::Text { text, rect, .. }
                        if rect.x < area.layout_cache.content_rect.x =>
                    {
                        Some(text)
                    }
                    _ => None,
                })
                .collect()
        };
        assert_eq!(painted_numbers, vec!["1", "2", "3", "4"]);

        area.selection_anchor = 2;
        area.selection_head = 2;
        area.relayout(measure_width);
        let _ = area.handle_event(UiEvent::KeyPressed {
            key: Key::Down,
            modifiers: Modifiers::default(),
        });
        assert_eq!(
            area.line_position_for_caret().map(|(line, _, _)| line),
            Some(1)
        );
        area.relayout(measure_width);
        let _ = area.handle_event(UiEvent::KeyPressed {
            key: Key::Down,
            modifiers: Modifiers::default(),
        });
        assert_eq!(
            area.line_position_for_caret().map(|(line, _, _)| line),
            Some(2)
        );
    }

    #[test]
    fn backspacing_at_start_of_empty_line_merges_lines_without_duplicate_numbers() {
        let mut area = area("one\ntwo\nthree\nfour");
        area.show_line_numbers = true;
        area.relayout(measure_width);

        area.selection_anchor = 7;
        area.selection_head = 7;
        for _ in 0..4 {
            let _ = area.handle_event(UiEvent::KeyPressed {
                key: Key::Backspace,
                modifiers: Modifiers::default(),
            });
        }
        area.relayout(measure_width);

        assert_eq!(area.text(), "one\nthree\nfour");
        assert_eq!(area.line_starts, vec![0, 4, 10]);
        assert_eq!(area.document_lines.len(), 3);
        assert_eq!(area.document_lines[0].display_text, "one");
        assert_eq!(area.document_lines[1].display_text, "three");
        assert_eq!(area.document_lines[2].display_text, "four");

        let painted_numbers: Vec<String> = {
            let mut ops = Vec::new();
            area.paint(&mut ops);
            ops.into_iter()
                .filter_map(|op| match op {
                    crate::paint::PaintOp::Text { text, rect, .. }
                        if rect.x < area.layout_cache.content_rect.x =>
                    {
                        Some(text)
                    }
                    _ => None,
                })
                .collect()
        };
        assert_eq!(painted_numbers, vec!["1", "2", "3"]);

        area.selection_anchor = 2;
        area.selection_head = 2;
        area.relayout(measure_width);
        let _ = area.handle_event(UiEvent::KeyPressed {
            key: Key::Down,
            modifiers: Modifiers::default(),
        });
        assert_eq!(
            area.line_position_for_caret().map(|(line, _, _)| line),
            Some(1)
        );
        area.relayout(measure_width);
        let _ = area.handle_event(UiEvent::KeyPressed {
            key: Key::Down,
            modifiers: Modifiers::default(),
        });
        assert_eq!(
            area.line_position_for_caret().map(|(line, _, _)| line),
            Some(2)
        );
    }
}
