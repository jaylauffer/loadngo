//! What a terminal sends for a key: the sequences xterm uses, which
//! shells and full-screen programs expect.

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

    #[test]
    fn pastes_are_bracketed_on_request() {
        let mut out = Vec::new();
        encode_paste("a\r\nb\n\x1b[201~c", true, &mut out);
        assert_eq!(out, b"\x1b[200~a\rb\r[201~c\x1b[201~");
    }
}
