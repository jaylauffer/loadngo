//! What a terminal sends for a key: the sequences xterm uses, which
//! shells and full-screen programs expect, and the reports for pointer
//! events programs ask for.

use crate::{MouseEncoding, MouseTracking};

/// Keys that are not text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Backspace,
    Enter,
    Tab,
    Escape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub shift: bool,
    /// Option on macOS.
    pub alt: bool,
    pub ctrl: bool,
}

impl Modifiers {
    /// xterm's modifier parameter: 1 plus shift 1, alt 2, ctrl 4.
    fn parameter(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }

    fn any(self) -> bool {
        self.shift || self.alt || self.ctrl
    }
}

/// Appends what `key` sends to `out`. `app_cursor` is the program's cursor
/// key mode ([`Modes::app_cursor`](crate::Modes)).
pub fn encode_key(key: Key, modifiers: Modifiers, app_cursor: bool, out: &mut Vec<u8>) {
    let arrow = |letter: u8, out: &mut Vec<u8>| {
        if modifiers.any() {
            out.extend_from_slice(format!("\x1b[1;{}", modifiers.parameter()).as_bytes());
            out.push(letter);
        } else if app_cursor {
            out.extend_from_slice(&[0x1b, b'O', letter]);
        } else {
            out.extend_from_slice(&[0x1b, b'[', letter]);
        }
    };
    let tilde = |number: u8, out: &mut Vec<u8>| {
        if modifiers.any() {
            out.extend_from_slice(format!("\x1b[{number};{}~", modifiers.parameter()).as_bytes());
        } else {
            out.extend_from_slice(format!("\x1b[{number}~").as_bytes());
        }
    };
    match key {
        Key::Up => arrow(b'A', out),
        Key::Down => arrow(b'B', out),
        // Option-arrows move by word in shells, as most macOS terminals
        // send them.
        Key::Right if modifiers.alt && !modifiers.ctrl && !modifiers.shift => {
            out.extend_from_slice(b"\x1bf")
        }
        Key::Left if modifiers.alt && !modifiers.ctrl && !modifiers.shift => {
            out.extend_from_slice(b"\x1bb")
        }
        Key::Right => arrow(b'C', out),
        Key::Left => arrow(b'D', out),
        Key::Home => arrow(b'H', out),
        Key::End => arrow(b'F', out),
        Key::PageUp => tilde(5, out),
        Key::PageDown => tilde(6, out),
        Key::Delete => tilde(3, out),
        Key::Backspace => {
            if modifiers.alt {
                out.push(0x1b);
            }
            out.push(if modifiers.ctrl { 0x08 } else { 0x7f });
        }
        Key::Enter => {
            if modifiers.alt {
                out.push(0x1b);
            }
            out.push(b'\r');
        }
        Key::Tab if modifiers.shift => out.extend_from_slice(b"\x1b[Z"),
        Key::Tab => out.push(b'\t'),
        Key::Escape => out.push(0x1b),
    }
}

/// A pointer button, as mouse reports name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    /// Movement with no button held.
    None,
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
}

impl MouseButton {
    fn code(self) -> u32 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
            Self::None => 3,
            Self::WheelUp => 64,
            Self::WheelDown => 65,
            Self::WheelLeft => 66,
            Self::WheelRight => 67,
        }
    }

    fn is_wheel(self) -> bool {
        matches!(
            self,
            Self::WheelUp | Self::WheelDown | Self::WheelLeft | Self::WheelRight
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    /// The pointer moved to another cell; `button` is the one held, or
    /// [`MouseButton::None`].
    Motion,
}

/// A pointer event on the screen; `col` and `row` count from 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub action: MouseAction,
    pub button: MouseButton,
    pub col: usize,
    pub row: usize,
    pub modifiers: Modifiers,
}

/// Appends the report a program asked for with `tracking` and `encoding`
/// ([`Modes::mouse`](crate::Modes)). Returns false, appending nothing,
/// when the program did not ask for this kind of event (or, in the legacy
/// encoding, the position is past column or row 223).
pub fn encode_mouse(
    event: MouseEvent,
    tracking: MouseTracking,
    encoding: MouseEncoding,
    out: &mut Vec<u8>,
) -> bool {
    let wanted = match (event.action, tracking) {
        (_, MouseTracking::Off) => false,
        (MouseAction::Press, _) => event.button != MouseButton::None,
        (MouseAction::Release, MouseTracking::Press) => false,
        // The wheel has no releases.
        (MouseAction::Release, _) => !event.button.is_wheel(),
        (MouseAction::Motion, MouseTracking::Motion) => true,
        (MouseAction::Motion, MouseTracking::Drag) => {
            event.button != MouseButton::None && !event.button.is_wheel()
        }
        (MouseAction::Motion, _) => false,
    };
    if !wanted {
        return false;
    }
    let mut code = event.button.code();
    // Only SGR says which button was released.
    if event.action == MouseAction::Release && encoding != MouseEncoding::Sgr {
        code = 3;
    }
    if event.action == MouseAction::Motion {
        code += 32;
    }
    if tracking != MouseTracking::Press {
        let modifiers = event.modifiers;
        code += 4 * u32::from(modifiers.shift)
            + 8 * u32::from(modifiers.alt)
            + 16 * u32::from(modifiers.ctrl);
    }
    let (col, row) = (event.col + 1, event.row + 1);
    match encoding {
        MouseEncoding::Sgr => {
            let end = if event.action == MouseAction::Release {
                'm'
            } else {
                'M'
            };
            out.extend_from_slice(format!("\x1b[<{code};{col};{row}{end}").as_bytes());
        }
        MouseEncoding::Urxvt => {
            out.extend_from_slice(format!("\x1b[{};{col};{row}M", code + 32).as_bytes());
        }
        MouseEncoding::Legacy => {
            let (Ok(col), Ok(row)) = (u8::try_from(col + 32), u8::try_from(row + 32)) else {
                return false;
            };
            out.extend_from_slice(&[0x1b, b'[', b'M', (code + 32) as u8, col, row]);
        }
        MouseEncoding::Utf8 => {
            let (Some(col), Some(row)) = (
                u32::try_from(col + 32).ok().and_then(char::from_u32),
                u32::try_from(row + 32).ok().and_then(char::from_u32),
            ) else {
                return false;
            };
            // UTF-8 reaches 2047 in two bytes; xterm stops there.
            if u32::from(col) > 2047 || u32::from(row) > 2047 {
                return false;
            }
            out.extend_from_slice(b"\x1b[M");
            for ch in [char::from(code as u8 + 32), col, row] {
                let mut buffer = [0; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
            }
        }
    }
    true
}

/// Appends pasted text: line breaks as Enter (`\r`), wrapped in the
/// bracketed-paste markers when the program asked for them. Escape
/// characters in the text are dropped, so a paste cannot end the
/// bracketing early.
pub fn encode_paste(text: &str, bracketed: bool, out: &mut Vec<u8>) {
    if bracketed {
        out.extend_from_slice(b"\x1b[200~");
    }
    let mut previous_cr = false;
    for ch in text.chars() {
        match ch {
            '\x1b' => {}
            '\n' if previous_cr => {}
            '\n' | '\r' => out.push(b'\r'),
            _ => {
                let mut buffer = [0; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
            }
        }
        previous_cr = ch == '\r';
    }
    if bracketed {
        out.extend_from_slice(b"\x1b[201~");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyed(key: Key, modifiers: Modifiers, app: bool) -> Vec<u8> {
        let mut out = Vec::new();
        encode_key(key, modifiers, app, &mut out);
        out
    }

    #[test]
    fn keys_send_xterm_sequences() {
        let none = Modifiers::default();
        let shift = Modifiers {
            shift: true,
            ..none
        };
        let alt = Modifiers { alt: true, ..none };
        assert_eq!(keyed(Key::Up, none, false), b"\x1b[A");
        assert_eq!(keyed(Key::Up, none, true), b"\x1bOA");
        assert_eq!(keyed(Key::Right, shift, true), b"\x1b[1;2C");
        assert_eq!(keyed(Key::Left, alt, false), b"\x1bb");
        assert_eq!(keyed(Key::Delete, none, false), b"\x1b[3~");
        assert_eq!(keyed(Key::PageDown, shift, false), b"\x1b[6;2~");
        assert_eq!(keyed(Key::Backspace, alt, false), b"\x1b\x7f");
        assert_eq!(keyed(Key::Tab, shift, false), b"\x1b[Z");
    }

    fn reported(
        action: MouseAction,
        button: MouseButton,
        (col, row): (usize, usize),
        modifiers: Modifiers,
        tracking: MouseTracking,
        encoding: MouseEncoding,
    ) -> Option<Vec<u8>> {
        let event = MouseEvent {
            action,
            button,
            col,
            row,
            modifiers,
        };
        let mut out = Vec::new();
        encode_mouse(event, tracking, encoding, &mut out).then_some(out)
    }

    #[test]
    fn mouse_reports_follow_the_programs_tracking_mode() {
        use MouseAction::*;
        use MouseButton as B;
        let none = Modifiers::default();
        let sgr = MouseEncoding::Sgr;
        let at = (4, 2);
        for tracking in [
            MouseTracking::Press,
            MouseTracking::Click,
            MouseTracking::Drag,
            MouseTracking::Motion,
        ] {
            assert_eq!(
                reported(Press, B::Left, at, none, tracking, sgr).as_deref(),
                Some(&b"\x1b[<0;5;3M"[..]),
                "{tracking:?}"
            );
        }
        let off = MouseTracking::Off;
        assert_eq!(reported(Press, B::Left, at, none, off, sgr), None);
        let press = MouseTracking::Press;
        assert_eq!(reported(Release, B::Left, at, none, press, sgr), None);
        let click = MouseTracking::Click;
        assert_eq!(
            reported(Release, B::Right, at, none, click, sgr).as_deref(),
            Some(&b"\x1b[<2;5;3m"[..])
        );
        assert_eq!(reported(Motion, B::Left, at, none, click, sgr), None);
        assert_eq!(reported(Release, B::WheelUp, at, none, click, sgr), None);
        assert_eq!(
            reported(Press, B::WheelDown, at, none, click, sgr).as_deref(),
            Some(&b"\x1b[<65;5;3M"[..])
        );
        let drag = MouseTracking::Drag;
        assert_eq!(
            reported(Motion, B::Left, at, none, drag, sgr).as_deref(),
            Some(&b"\x1b[<32;5;3M"[..])
        );
        assert_eq!(reported(Motion, B::None, at, none, drag, sgr), None);
        let motion = MouseTracking::Motion;
        assert_eq!(
            reported(Motion, B::None, at, none, motion, sgr).as_deref(),
            Some(&b"\x1b[<35;5;3M"[..])
        );
        assert_eq!(reported(Press, B::None, at, none, motion, sgr), None);
    }

    #[test]
    fn mouse_reports_carry_modifiers_in_every_encoding() {
        use MouseAction::*;
        use MouseButton as B;
        let shift_ctrl = Modifiers {
            shift: true,
            ctrl: true,
            ..Modifiers::default()
        };
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        let click = MouseTracking::Click;
        assert_eq!(
            reported(
                Press,
                B::Middle,
                (0, 0),
                shift_ctrl,
                click,
                MouseEncoding::Sgr
            )
            .as_deref(),
            Some(&b"\x1b[<21;1;1M"[..])
        );
        // X10 reports no modifiers.
        assert_eq!(
            reported(
                Press,
                B::Left,
                (0, 0),
                alt,
                MouseTracking::Press,
                MouseEncoding::Sgr
            )
            .as_deref(),
            Some(&b"\x1b[<0;1;1M"[..])
        );
        assert_eq!(
            reported(Press, B::Left, (9, 19), alt, click, MouseEncoding::Legacy).as_deref(),
            Some(&[0x1b, b'[', b'M', 32 + 8, 32 + 10, 32 + 20][..])
        );
        assert_eq!(
            reported(Release, B::Left, (9, 19), alt, click, MouseEncoding::Legacy).as_deref(),
            Some(&[0x1b, b'[', b'M', 32 + 11, 32 + 10, 32 + 20][..])
        );
        assert_eq!(
            reported(Release, B::Left, (9, 19), alt, click, MouseEncoding::Urxvt).as_deref(),
            Some(&b"\x1b[43;10;20M"[..])
        );
        // Past column 223 the legacy encoding has no byte to say it.
        let far = (300, 1);
        assert_eq!(
            reported(
                Press,
                B::Left,
                far,
                Modifiers::default(),
                click,
                MouseEncoding::Legacy
            ),
            None
        );
        let mut expected = b"\x1b[M ".to_vec();
        expected.extend_from_slice("\u{14d}\"".as_bytes());
        assert_eq!(
            reported(
                Press,
                B::Left,
                far,
                Modifiers::default(),
                click,
                MouseEncoding::Utf8
            ),
            Some(expected)
        );
    }

    #[test]
    fn pastes_are_bracketed_on_request() {
        let mut out = Vec::new();
        encode_paste("a\r\nb\n\x1b[201~c", true, &mut out);
        assert_eq!(out, b"\x1b[200~a\rb\r[201~c\x1b[201~");
    }
}
