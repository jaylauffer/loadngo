//! One line of typed text, which can be hidden: a master password, a name.
//!
//! `ui_core::TextFieldModel` is the general single-line field. This one is
//! for secrets, which that one is not: it keeps no undo history, hands its
//! text to no clipboard, draws dots in place of hidden text, and wipes what
//! it held when it is cleared or dropped.

use loadngo_host_core::{HostKey, HostKeyEvent};
use ui_core::{
    Color, HorizontalAlign, PaintOp, Point, Rect, TextLayoutMode, TextOverflow, TextStyle,
    TextVerticalMetricMode, VerticalAlign,
};
use zeroize::Zeroize;

use crate::draw;

/// How wide `text` is at a font size.
pub type Measure = fn(&str, u16) -> f32;

/// A stand-in for the host's measuring, for tests and before a host is
/// there.
pub fn roughly(text: &str, font_size: u16) -> f32 {
    text.chars().count() as f32 * f32::from(font_size) * 0.55
}

/// Room between the box and what is typed in it.
const INSET: f32 = 10.0;
/// From one dot of a hidden text to the next.
const DOT_PITCH: f32 = 10.0;
const DOT_RADIUS: f32 = 3.2;
/// The most characters a field takes: more than any password a site accepts.
const MOST: usize = 256;

#[derive(Default)]
pub struct Field {
    text: String,
    /// The caret, as a count of characters before it.
    caret: usize,
    /// Everything is selected: the next thing typed replaces it.
    all: bool,
    /// Dots are drawn in place of the text.
    pub hidden: bool,
}

impl Drop for Field {
    fn drop(&mut self) {
        self.text.zeroize();
    }
}

impl std::fmt::Debug for Field {
    /// Never what was typed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Field")
            .field("characters", &self.len())
            .field("hidden", &self.hidden)
            .finish()
    }
}

impl Field {
    pub fn hidden() -> Self {
        let mut field = Self::default();
        field.hidden = true;
        field
    }

    pub fn holding(text: &str) -> Self {
        let mut field = Self::default();
        field.set(text);
        field
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Replaces everything and puts the caret at the end.
    pub fn set(&mut self, text: &str) {
        self.text.zeroize();
        for character in text.chars().filter(|c| !c.is_control()).take(MOST) {
            self.text.push(character);
        }
        self.caret = self.len();
        self.all = false;
    }

    /// Wipes what it holds.
    pub fn clear(&mut self) {
        self.text.zeroize();
        self.caret = 0;
        self.all = false;
    }

    pub fn select_all(&mut self) {
        self.all = !self.text.is_empty();
        self.caret = self.len();
    }

    /// Where the `count`th character starts, in bytes.
    fn byte_at(&self, count: usize) -> usize {
        self.text
            .char_indices()
            .nth(count)
            .map_or(self.text.len(), |(at, _)| at)
    }

    /// Puts typed or pasted text in at the caret. Line breaks, tabs and
    /// other control characters are left out. Says whether anything changed.
    pub fn insert(&mut self, text: &str) -> bool {
        let mut changed = false;
        for character in text.chars().filter(|c| !c.is_control()) {
            if self.all {
                self.clear();
                changed = true;
            }
            if self.len() >= MOST {
                break;
            }
            let at = self.byte_at(self.caret);
            self.text.insert(at, character);
            self.caret += 1;
            changed = true;
        }
        changed
    }

    /// A key pressed with the field in focus. Says whether the text changed.
    /// Enter, Tab and Escape are not the field's.
    pub fn key(&mut self, event: &HostKeyEvent) -> bool {
        let command = event.modifiers.meta || event.modifiers.ctrl;
        match event.key {
            HostKey::A if command => {
                self.select_all();
                false
            }
            HostKey::Backspace | HostKey::Delete if self.all => {
                self.clear();
                true
            }
            HostKey::Backspace if self.caret > 0 => {
                let from = self.byte_at(self.caret - 1);
                let to = self.byte_at(self.caret);
                self.wipe(from, to);
                self.caret -= 1;
                true
            }
            HostKey::Delete if self.caret < self.len() => {
                let from = self.byte_at(self.caret);
                let to = self.byte_at(self.caret + 1);
                self.wipe(from, to);
                true
            }
            HostKey::Left => {
                self.caret = if self.all || command {
                    0
                } else {
                    self.caret.saturating_sub(1)
                };
                self.all = false;
                false
            }
            HostKey::Right => {
                self.caret = if self.all || command {
                    self.len()
                } else {
                    (self.caret + 1).min(self.len())
                };
                self.all = false;
                false
            }
            HostKey::Home | HostKey::Up => {
                self.caret = 0;
                self.all = false;
                false
            }
            HostKey::End | HostKey::Down => {
                self.caret = self.len();
                self.all = false;
                false
            }
            _ => false,
        }
    }

    /// Takes out the bytes `from..to`, leaving no copy of them past the
    /// end of what remains.
    fn wipe(&mut self, from: usize, to: usize) {
        let mut rest = self.text.split_off(to);
        let mut gone = self.text.split_off(from);
        gone.zeroize();
        self.text.push_str(&rest);
        rest.zeroize();
    }

    /// How far the caret is from where the text starts.
    fn caret_x(&self, measure: Measure, font_size: u16) -> f32 {
        if self.hidden {
            self.caret as f32 * DOT_PITCH
        } else {
            measure(&self.text[..self.byte_at(self.caret)], font_size)
        }
    }

    /// How far the text is slid left so that the caret is inside `room`.
    fn slide(&self, room: f32, measure: Measure, font_size: u16) -> f32 {
        (self.caret_x(measure, font_size) - room).max(0.0)
    }

    /// A press at `point`, inside the field's `rect`: the caret goes to the
    /// nearest gap between characters.
    pub fn press(&mut self, point: Point, rect: Rect, measure: Measure, font_size: u16) {
        let room = (rect.width - 2.0 * INSET).max(0.0);
        let x = point.x - rect.x - INSET + self.slide(room, measure, font_size);
        let count = self.len();
        self.all = false;
        self.caret = if self.hidden {
            ((x / DOT_PITCH).round().max(0.0) as usize).min(count)
        } else {
            let mut best = (f32::MAX, 0);
            for gap in 0..=count {
                let at = measure(&self.text[..self.byte_at(gap)], font_size);
                if (at - x).abs() < best.0 {
                    best = ((at - x).abs(), gap);
                }
            }
            best.1
        };
    }

    /// Draws the field in `rect`. A hidden field's text is in nothing that
    /// is drawn.
    pub fn paint(
        &self,
        scene: &mut Vec<PaintOp>,
        rect: Rect,
        focused: bool,
        measure: Measure,
        font_size: u16,
    ) {
        draw::fill(scene, rect, draw::BACKGROUND);
        draw::stroke(scene, rect, if focused { draw::ACCENT } else { draw::EDGE });
        let room = Rect {
            x: rect.x + INSET,
            y: rect.y + 1.0,
            width: (rect.width - 2.0 * INSET).max(0.0),
            height: (rect.height - 2.0).max(0.0),
        };
        if room.width <= 0.0 {
            return;
        }
        let slide = self.slide(room.width, measure, font_size);
        let middle = rect.y + rect.height / 2.0;
        if self.all && focused {
            let width = if self.hidden {
                self.len() as f32 * DOT_PITCH
            } else {
                measure(&self.text, font_size)
            };
            draw::fill(
                scene,
                Rect {
                    x: room.x,
                    y: rect.y + 5.0,
                    width: (width - slide).clamp(0.0, room.width),
                    height: (rect.height - 10.0).max(0.0),
                },
                Color::rgba(0x4f, 0x8f, 0xe8, 0x66),
            );
        }
        if self.hidden {
            for index in 0..self.len() {
                let x = room.x + (index as f32 + 0.5) * DOT_PITCH - slide;
                if x - DOT_RADIUS >= room.x && x + DOT_RADIUS <= room.right() {
                    draw::dot(scene, Point { x, y: middle }, DOT_RADIUS, draw::TEXT);
                }
            }
        } else if !self.text.is_empty() {
            scene.push(PaintOp::Text {
                rect: Rect {
                    x: room.x - slide,
                    width: room.width + slide + 4.0,
                    ..room
                },
                clip_rect: Some(room),
                text: self.text.clone(),
                style: TextStyle {
                    color: draw::TEXT,
                    font_size,
                    horizontal_align: HorizontalAlign::Left,
                    vertical_align: VerticalAlign::Middle,
                    vertical_metric_mode: TextVerticalMetricMode::LogicalLineBox,
                    layout_mode: TextLayoutMode::SingleLine,
                    overflow: TextOverflow::Clip,
                },
            });
        }
        if focused && !self.all {
            let x = (room.x + self.caret_x(measure, font_size) - slide).min(room.right());
            draw::fill(
                scene,
                Rect {
                    x,
                    y: rect.y + 6.0,
                    width: 1.5,
                    height: (rect.height - 12.0).max(0.0),
                },
                draw::TEXT,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui_core::Modifiers;

    fn key(field: &mut Field, key: HostKey) -> bool {
        field.key(&HostKeyEvent {
            key,
            modifiers: Modifiers::default(),
        })
    }

    fn command(field: &mut Field, key: HostKey) -> bool {
        field.key(&HostKeyEvent {
            key,
            modifiers: Modifiers {
                meta: true,
                ..Modifiers::default()
            },
        })
    }

    #[test]
    fn typing_goes_in_at_the_caret_and_the_keys_move_and_remove() {
        let mut field = Field::default();
        assert!(field.insert("héllo"));
        assert_eq!(field.text(), "héllo");
        key(&mut field, HostKey::Left);
        key(&mut field, HostKey::Left);
        assert!(field.insert("X"));
        assert_eq!(field.text(), "hélXlo");
        assert!(key(&mut field, HostKey::Backspace));
        assert!(key(&mut field, HostKey::Backspace));
        assert_eq!(field.text(), "hélo");
        assert!(key(&mut field, HostKey::Delete));
        assert_eq!(field.text(), "héo");
        key(&mut field, HostKey::Home);
        assert!(!key(&mut field, HostKey::Backspace));
        assert!(key(&mut field, HostKey::Delete));
        assert_eq!(field.text(), "éo");
        key(&mut field, HostKey::End);
        assert!(!key(&mut field, HostKey::Delete));
        assert!(field.insert("!"));
        assert_eq!(field.text(), "éo!");
    }

    #[test]
    fn line_breaks_and_control_characters_are_left_out() {
        let mut field = Field::default();
        assert!(field.insert("a\nb\tc\r\u{8}d\u{7f}"));
        assert_eq!(field.text(), "abcd");
        assert!(!field.insert("\n"));
        field.set("x\ny");
        assert_eq!(field.text(), "xy");
    }

    #[test]
    fn with_everything_selected_typing_replaces_and_backspace_empties() {
        let mut field = Field::holding("old text");
        command(&mut field, HostKey::A);
        assert!(field.insert("n"));
        assert!(field.insert("ew"));
        assert_eq!(field.text(), "new");
        command(&mut field, HostKey::A);
        assert!(key(&mut field, HostKey::Backspace));
        assert!(field.is_empty());
        // Nothing to select in an empty field.
        command(&mut field, HostKey::A);
        assert!(!key(&mut field, HostKey::Backspace));
        // An arrow lets go of the selection without changing the text.
        field.set("abc");
        command(&mut field, HostKey::A);
        key(&mut field, HostKey::Left);
        assert!(field.insert("-"));
        assert_eq!(field.text(), "-abc");
    }

    #[test]
    fn a_field_takes_no_more_than_it_can_hold() {
        let mut field = Field::default();
        field.insert(&"x".repeat(1000));
        assert_eq!(field.len(), MOST);
    }

    #[test]
    fn a_press_puts_the_caret_at_the_nearest_gap() {
        let rect = Rect {
            x: 100.0,
            y: 0.0,
            width: 300.0,
            height: 32.0,
        };
        // Shown: `roughly` makes each character 7.15 wide at size 13.
        let mut field = Field::holding("abcdef");
        let at = |x: f32| Point { x, y: 16.0 };
        field.press(at(100.0 + INSET + 7.15 * 2.0 + 1.0), rect, roughly, 13);
        field.insert("|");
        assert_eq!(field.text(), "ab|cdef");
        field.press(at(390.0), rect, roughly, 13);
        field.insert("$");
        assert_eq!(field.text(), "ab|cdef$");
        field.press(at(100.0), rect, roughly, 13);
        field.insert("^");
        assert_eq!(field.text(), "^ab|cdef$");
        // Hidden: a dot every DOT_PITCH.
        let mut field = Field::hidden();
        field.insert("abcdef");
        field.press(at(100.0 + INSET + DOT_PITCH * 4.0 - 2.0), rect, roughly, 13);
        field.insert("|");
        assert_eq!(field.text(), "abcd|ef");
    }

    #[test]
    fn a_hidden_field_draws_dots_and_never_its_text() {
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 300.0,
            height: 32.0,
        };
        let mut field = Field::hidden();
        field.insert("hunter2");
        let mut scene = Vec::new();
        field.paint(&mut scene, rect, true, roughly, 13);
        let dots = scene
            .iter()
            .filter(|op| matches!(op, PaintOp::FillCircle { .. }))
            .count();
        assert_eq!(dots, 7);
        assert!(!scene.iter().any(|op| matches!(op, PaintOp::Text { .. })));
        assert!(!format!("{scene:?}").contains("hunter2"));
        assert!(!format!("{field:?}").contains("hunter2"));
        // Shown, the text is drawn and clipped to the box.
        field.hidden = false;
        scene.clear();
        field.paint(&mut scene, rect, false, roughly, 13);
        assert!(scene.iter().any(|op| matches!(
            op,
            PaintOp::Text { text, clip_rect: Some(_), .. } if text == "hunter2"
        )));
    }

    #[test]
    fn a_long_hidden_text_keeps_its_caret_in_the_box() {
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 120.0,
            height: 32.0,
        };
        let mut field = Field::hidden();
        field.insert(&"x".repeat(40));
        let mut scene = Vec::new();
        field.paint(&mut scene, rect, true, roughly, 13);
        for op in &scene {
            if let PaintOp::FillCircle { center, radius, .. } = op {
                assert!(center.x - radius >= INSET && center.x + radius <= 120.0 - INSET);
            }
        }
        // Ten dots fit in the hundred units between the insets.
        let dots = scene
            .iter()
            .filter(|op| matches!(op, PaintOp::FillCircle { .. }))
            .count();
        assert_eq!(dots, 10);
    }
}
