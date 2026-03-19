use serde::{Deserialize, Serialize};

use crate::geometry::Point;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointerSource {
    Mouse,
    Touch,
    Pen,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointerButton {
    Primary,
    Secondary,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PointerState {
    pub id: u64,
    pub position: Point,
    pub source: PointerSource,
    pub modifiers: Modifiers,
}

impl Default for PointerState {
    fn default() -> Self {
        Self {
            id: 0,
            position: Point::default(),
            source: PointerSource::Mouse,
            modifiers: Modifiers::default(),
        }
    }
}

impl PointerState {
    pub fn new(id: u64, position: Point, source: PointerSource, modifiers: Modifiers) -> Self {
        Self {
            id,
            position,
            source,
            modifiers,
        }
    }

    pub fn mouse(position: Point, modifiers: Modifiers) -> Self {
        Self::new(0, position, PointerSource::Mouse, modifiers)
    }

    pub fn touch(id: u64, position: Point) -> Self {
        Self::new(id, position, PointerSource::Touch, Modifiers::default())
    }

    pub fn pen(id: u64, position: Point, modifiers: Modifiers) -> Self {
        Self::new(id, position, PointerSource::Pen, modifiers)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Key {
    Enter,
    Space,
    Escape,
    Tab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Backspace,
    Delete,
    Character(char),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UiEvent {
    PointerMoved(PointerState),
    PointerLeft,
    PointerPressed {
        button: PointerButton,
        state: PointerState,
    },
    PointerReleased {
        button: PointerButton,
        state: PointerState,
    },
    FocusChanged(bool),
    KeyPressed {
        key: Key,
        modifiers: Modifiers,
    },
    TextInput {
        text: String,
    },
    ScrollLines {
        delta: i32,
    },
}

#[cfg(test)]
mod tests {
    use super::{Modifiers, PointerSource, PointerState};
    use crate::Point;

    #[test]
    fn mouse_pointer_uses_stable_primary_id() {
        let pointer = PointerState::mouse(Point { x: 4.0, y: 7.0 }, Modifiers::default());
        assert_eq!(pointer.id, 0);
        assert_eq!(pointer.source, PointerSource::Mouse);
    }

    #[test]
    fn touch_pointer_preserves_contact_id() {
        let pointer = PointerState::touch(42, Point { x: 12.0, y: 18.0 });
        assert_eq!(pointer.id, 42);
        assert_eq!(pointer.source, PointerSource::Touch);
    }
}
