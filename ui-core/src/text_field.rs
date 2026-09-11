//! Single-line editable text: a file name, a search box, a numeric entry.
//!
//! Built on [`TextAreaModel`] rather than beside it, so a field gets the same
//! piece-table document, caret, selection, undo/redo, and pointer placement
//! the multiline editor already has. What makes it single-line is entirely in
//! how events are filtered on the way in:
//!
//! - `Enter` does not insert a newline; it emits [`WidgetAction::Activate`]
//!   so a form can submit.
//! - `Tab`, `Up`, and `Down` are not consumed, so a host can move focus or
//!   drive a neighbouring list with them.
//! - Pasted or typed text has line breaks and tabs stripped.
//! - Wheel scrolling is ignored; there is no second line to scroll to.

use crate::{
    geometry::{Point, Rect},
    input::{Key, UiEvent},
    paint::PaintOp,
    text::single_line_text_box_height,
    text_area::TextAreaModel,
    widget::{WidgetAction, WidgetId, WidgetResponse},
};

#[derive(Debug, Clone, PartialEq)]
pub struct TextFieldModel {
    /// The editor underneath. Public so callers can style it (colors,
    /// font size, padding) exactly as they would a `TextAreaModel`.
    pub area: TextAreaModel,
}

impl TextFieldModel {
    pub fn new(text: impl Into<String>, bounds: Rect) -> Self {
        Self::with_id(WidgetId(0), text, bounds)
    }

    pub fn with_id(widget_id: WidgetId, text: impl Into<String>, bounds: Rect) -> Self {
        let text = sanitize(&text.into());
        let mut area = TextAreaModel::with_id(widget_id, text, bounds);
        area.padding.top = 0.0;
        area.padding.bottom = 0.0;
        area.set_caret(usize::MAX);
        let mut field = Self { area };
        field.center_vertically();
        field
    }

    pub fn widget_id(&self) -> WidgetId {
        self.area.widget_id
    }

    pub fn bounds(&self) -> Rect {
        self.area.bounds
    }

    pub fn set_bounds(&mut self, bounds: Rect) {
        self.area.set_bounds(bounds);
        self.center_vertically();
    }

    pub fn text(&self) -> String {
        self.area.text()
    }

    pub fn focused(&self) -> bool {
        self.area.focused
    }

    /// Replaces the whole contents and puts the caret at the end. Styling,
    /// bounds, and focus are kept; undo history is not, since this is a
    /// programmatic reset (e.g. a dialog filling in a selected file name)
    /// rather than an edit the user would expect to undo.
    pub fn set_text(&mut self, text: &str) {
        let mut next =
            TextAreaModel::with_id(self.area.widget_id, sanitize(text), self.area.bounds);
        next.style = self.area.style.clone();
        next.padding = self.area.padding;
        next.background = self.area.background;
        next.border = self.area.border;
        next.selection_fill = self.area.selection_fill;
        next.caret_color = self.area.caret_color;
        next.focused = self.area.focused;
        next.show_caret = self.area.show_caret;
        next.hover = self.area.hover;
        next.set_caret(usize::MAX);
        self.area = next;
    }

    /// Selects `start..end` (character offsets), e.g. a file name's stem so
    /// typing replaces the name but keeps the extension.
    pub fn select_range(&mut self, start: usize, end: usize) {
        self.area.set_caret(start);
        self.area.selection_head = end.max(start).min(self.area.text().chars().count());
    }

    pub fn select_all(&mut self) {
        self.area.select_all();
    }

    pub fn prefers_text_cursor(&self, point: Point) -> bool {
        self.area.prefers_text_cursor(point)
    }

    /// Must be called after bounds, text, or font size change and before
    /// painting, with a text-width measurer from the host.
    pub fn relayout<F>(&mut self, measure_width: F)
    where
        F: FnMut(&str, u16) -> f32,
    {
        self.center_vertically();
        self.area.relayout(measure_width);
    }

    pub fn handle_event(&mut self, event: UiEvent) -> WidgetResponse {
        match event {
            UiEvent::KeyPressed {
                key: Key::Enter, ..
            } => {
                if !self.area.focused {
                    return WidgetResponse::default();
                }
                WidgetResponse {
                    request_redraw: true,
                    request_focus: false,
                    input_consumed: true,
                    action: Some(WidgetAction::Activate(self.area.widget_id)),
                }
            }
            UiEvent::KeyPressed {
                key: Key::Tab | Key::Up | Key::Down,
                ..
            }
            | UiEvent::ScrollLines { .. } => WidgetResponse::default(),
            UiEvent::TextInput { text } => {
                let text = sanitize(&text);
                if text.is_empty() {
                    return WidgetResponse::default();
                }
                self.area.handle_event(UiEvent::TextInput { text })
            }
            other => self.area.handle_event(other),
        }
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        self.area.paint(scene);
    }

    fn center_vertically(&mut self) {
        let line = single_line_text_box_height(self.area.style.font_size);
        let spare = (self.area.bounds.height - line).max(0.0);
        self.area.padding.top = spare / 2.0;
        self.area.padding.bottom = spare / 2.0;
    }
}

/// Line breaks and tabs have no meaning in a single-line field; drop them
/// rather than letting a paste smuggle a second line in.
fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|ch| !matches!(ch, '\n' | '\r' | '\t'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::TextFieldModel;
    use crate::{
        geometry::{Point, Rect},
        input::{Key, Modifiers, PointerButton, PointerState, UiEvent},
        widget::{WidgetAction, WidgetId},
    };

    fn bounds() -> Rect {
        Rect {
            x: 10.0,
            y: 10.0,
            width: 300.0,
            height: 32.0,
        }
    }

    fn measure(text: &str, font_size: u16) -> f32 {
        text.chars().count() as f32 * f32::from(font_size) * 0.5
    }

    fn focused_field(text: &str) -> TextFieldModel {
        let mut field = TextFieldModel::with_id(WidgetId(7), text, bounds());
        field.handle_event(UiEvent::FocusChanged(true));
        field.relayout(measure);
        field
    }

    fn key(key: Key) -> UiEvent {
        UiEvent::KeyPressed {
            key,
            modifiers: Modifiers::default(),
        }
    }

    #[test]
    fn enter_submits_instead_of_inserting_a_newline() {
        let mut field = focused_field("take-01.wav");
        let response = field.handle_event(key(Key::Enter));
        assert_eq!(response.action, Some(WidgetAction::Activate(WidgetId(7))));
        assert!(response.input_consumed);
        assert_eq!(field.text(), "take-01.wav");
    }

    #[test]
    fn enter_does_nothing_when_unfocused() {
        let mut field = TextFieldModel::with_id(WidgetId(7), "a", bounds());
        assert_eq!(field.handle_event(key(Key::Enter)).action, None);
    }

    #[test]
    fn typed_and_pasted_line_breaks_are_stripped() {
        let mut field = focused_field("");
        field.handle_event(UiEvent::TextInput {
            text: "bass\n\ttake\r".to_string(),
        });
        assert_eq!(field.text(), "basstake");
    }

    #[test]
    fn navigation_keys_are_left_for_the_host() {
        let mut field = focused_field("abc");
        for navigation in [Key::Tab, Key::Up, Key::Down] {
            let response = field.handle_event(key(navigation));
            assert!(!response.input_consumed, "{navigation:?} was consumed");
        }
        assert!(
            !field
                .handle_event(UiEvent::ScrollLines { delta: 3 })
                .input_consumed
        );
    }

    #[test]
    fn editing_keys_still_reach_the_editor() {
        let mut field = focused_field("abc");
        field.handle_event(key(Key::Backspace));
        field.relayout(measure);
        assert_eq!(field.text(), "ab");
        field.handle_event(key(Key::Home));
        field.handle_event(UiEvent::TextInput {
            text: "x".to_string(),
        });
        field.relayout(measure);
        assert_eq!(field.text(), "xab");
    }

    #[test]
    fn set_text_keeps_focus_and_places_caret_at_end() {
        let mut field = focused_field("old");
        field.set_text("new-name.wav");
        field.relayout(measure);
        assert!(field.focused());
        assert_eq!(field.area.caret(), "new-name.wav".chars().count());
        field.handle_event(UiEvent::TextInput {
            text: "!".to_string(),
        });
        assert_eq!(field.text(), "new-name.wav!");
    }

    #[test]
    fn select_range_lets_typing_replace_just_the_stem() {
        let mut field = focused_field("take-01.wav");
        field.select_range(0, 7);
        field.handle_event(UiEvent::TextInput {
            text: "solo".to_string(),
        });
        field.relayout(measure);
        assert_eq!(field.text(), "solo.wav");
    }

    #[test]
    fn clicking_focuses_the_field() {
        let mut field = TextFieldModel::new("abc", bounds());
        field.relayout(measure);
        let response = field.handle_event(UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state: PointerState::mouse(Point { x: 20.0, y: 20.0 }, Modifiers::default()),
        });
        assert!(response.request_focus);
        assert!(field.focused());
    }
}
