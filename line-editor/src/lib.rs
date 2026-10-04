//! Line editing for loadngo's interactive command-line tools.
//!
//! On a Unix terminal, [`LineEditor::read_line`] switches the terminal to raw mode for
//! the one line and decodes the keys itself:
//!
//! - **Moving:** Left/Right and Home/End (Ctrl-B/F and Ctrl-A/E too); a word at a time
//!   with Alt-Left/Right, Ctrl-Left/Right or Alt-B/F.
//! - **Deleting:** Backspace and Delete; Ctrl-W a word back; Ctrl-U to the start of the
//!   line; Ctrl-K to its end.
//! - **History:** Up/Down (Ctrl-P/N) step through the lines entered this session.
//! - **Finishing:** Enter submits; Ctrl-C abandons the line; Ctrl-D on an empty line
//!   ends input.
//!
//! The terminal's settings are restored when the line ends, even by a panic. When stdin
//! is not a terminal (a pipe or file), or on Windows for now, lines are read plainly.
//!
//! The keys ([`Keys`]) and the editing ([`Line`]) are pure, so they are tested without a
//! terminal.

use std::io::{self, Write};

/// A key, as decoded from a terminal's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
    Up,
    Down,
    /// Ctrl-W
    DeleteWordBack,
    /// Ctrl-U
    DeleteToStart,
    /// Ctrl-K
    DeleteToEnd,
    /// Ctrl-C
    Interrupt,
    /// Ctrl-D
    EndOfInput,
    /// A sequence this editor does not use.
    Ignored,
}

/// Decodes a terminal's input bytes into keys. Bytes are pushed as they arrive; an
/// escape sequence or a multi-byte character split across reads is completed by the
/// next push.
#[derive(Default)]
pub struct Keys {
    pending: Vec<u8>,
}

impl Keys {
    /// The keys `bytes` complete, in order.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Key> {
        self.pending.extend_from_slice(bytes);
        let mut keys = Vec::new();
        while let Some((key, used)) = decode(&self.pending) {
            keys.push(key);
            self.pending.drain(..used);
        }
        keys
    }

    /// Ends a lone Escape that no sequence followed.
    pub fn flush(&mut self) -> Vec<Key> {
        if self.pending == [0x1b] {
            self.pending.clear();
            return vec![Key::Ignored];
        }
        Vec::new()
    }
}

/// The first key in `b` and how many bytes it took; `None` when `b` holds only the start
/// of one.
fn decode(b: &[u8]) -> Option<(Key, usize)> {
    let first = *b.first()?;
    let key = match first {
        b'\r' | b'\n' => Key::Enter,
        0x7f | 0x08 => Key::Backspace,
        0x01 => Key::Home,
        0x02 => Key::Left,
        0x03 => Key::Interrupt,
        0x04 => Key::EndOfInput,
        0x05 => Key::End,
        0x06 => Key::Right,
        0x0b => Key::DeleteToEnd,
        0x0e => Key::Down,
        0x10 => Key::Up,
        0x15 => Key::DeleteToStart,
        0x17 => Key::DeleteWordBack,
        0x1b => return escape(b),
        c if c < 0x20 => Key::Ignored,
        _ => {
            let len = match first {
                0x00..=0x7f => 1,
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf7 => 4,
                _ => return Some((Key::Ignored, 1)),
            };
            if b.len() < len {
                return None;
            }
            return Some(match std::str::from_utf8(&b[..len]) {
                Ok(s) => (Key::Char(s.chars().next().expect("one char")), len),
                Err(_) => (Key::Ignored, 1),
            });
        }
    };
    Some((key, 1))
}

/// An escape sequence: ESC [ … final byte (CSI), ESC O x (SS3), or ESC x (Alt-x).
fn escape(b: &[u8]) -> Option<(Key, usize)> {
    let second = *b.get(1)?;
    match second {
        b'[' => {
            // Parameters and intermediates, then a final byte in @..~.
            let end = b[2..].iter().position(|&c| (0x40..=0x7e).contains(&c))? + 2;
            let params = &b[2..end];
            let key = match (params, b[end]) {
                (b"", b'A') => Key::Up,
                (b"", b'B') => Key::Down,
                (b"", b'C') => Key::Right,
                (b"", b'D') => Key::Left,
                (b"", b'H') | (b"1", b'~') | (b"7", b'~') => Key::Home,
                (b"", b'F') | (b"4", b'~') | (b"8", b'~') => Key::End,
                (b"3", b'~') => Key::Delete,
                // Ctrl or Alt with an arrow: a word at a time.
                (b"1;5" | b"1;3" | b"5" | b"3", b'C') => Key::WordRight,
                (b"1;5" | b"1;3" | b"5" | b"3", b'D') => Key::WordLeft,
                _ => Key::Ignored,
            };
            Some((key, end + 1))
        }
        b'O' => {
            let third = *b.get(2)?;
            let key = match third {
                b'A' => Key::Up,
                b'B' => Key::Down,
                b'C' => Key::Right,
                b'D' => Key::Left,
                b'H' => Key::Home,
                b'F' => Key::End,
                _ => Key::Ignored,
            };
            Some((key, 3))
        }
        b'b' => Some((Key::WordLeft, 2)),
        b'f' => Some((Key::WordRight, 2)),
        0x7f => Some((Key::DeleteWordBack, 2)),
        _ => Some((Key::Ignored, 2)),
    }
}

/// What a key did to the line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Editing,
    Submit,
    Interrupt,
    EndOfInput,
}

/// A line being edited: its characters, the cursor, and the session's history.
#[derive(Clone, Debug, Default)]
pub struct Line {
    text: Vec<char>,
    cursor: usize,
    history: Vec<String>,
    /// The history entry shown, and the line being written before history was entered.
    browsing: Option<(usize, Vec<char>)>,
}

impl Line {
    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    /// The cursor, in characters from the start.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Clears the line for the next one (the history stays).
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.browsing = None;
    }

    /// Adds a submitted line to the history (not a blank one, nor a repeat of the last).
    pub fn remember(&mut self, line: &str) {
        if !line.trim().is_empty() && self.history.last().map(String::as_str) != Some(line) {
            self.history.push(line.to_owned());
        }
    }

    fn is_word(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    fn word_start(&self) -> usize {
        let mut at = self.cursor;
        while at > 0 && !Self::is_word(self.text[at - 1]) {
            at -= 1;
        }
        while at > 0 && Self::is_word(self.text[at - 1]) {
            at -= 1;
        }
        at
    }

    fn word_end(&self) -> usize {
        let mut at = self.cursor;
        while at < self.text.len() && !Self::is_word(self.text[at]) {
            at += 1;
        }
        while at < self.text.len() && Self::is_word(self.text[at]) {
            at += 1;
        }
        at
    }

    fn show(&mut self, text: Vec<char>) {
        self.cursor = text.len();
        self.text = text;
    }

    pub fn apply(&mut self, key: Key) -> Outcome {
        match key {
            Key::Char(c) => {
                self.text.insert(self.cursor, c);
                self.cursor += 1;
            }
            Key::Enter => return Outcome::Submit,
            Key::Interrupt => return Outcome::Interrupt,
            Key::EndOfInput if self.text.is_empty() => return Outcome::EndOfInput,
            Key::EndOfInput | Key::Delete => {
                if self.cursor < self.text.len() {
                    self.text.remove(self.cursor);
                }
            }
            Key::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.text.remove(self.cursor);
                }
            }
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => self.cursor = (self.cursor + 1).min(self.text.len()),
            Key::WordLeft => self.cursor = self.word_start(),
            Key::WordRight => self.cursor = self.word_end(),
            Key::Home => self.cursor = 0,
            Key::End => self.cursor = self.text.len(),
            Key::DeleteWordBack => {
                let from = self.word_start();
                self.text.drain(from..self.cursor);
                self.cursor = from;
            }
            Key::DeleteToStart => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
            }
            Key::DeleteToEnd => {
                self.text.truncate(self.cursor);
            }
            Key::Up => {
                let next = match &self.browsing {
                    None if !self.history.is_empty() => Some(self.history.len() - 1),
                    Some((at, _)) if *at > 0 => Some(at - 1),
                    _ => None,
                };
                if let Some(at) = next {
                    let draft = match self.browsing.take() {
                        Some((_, draft)) => draft,
                        None => self.text.clone(),
                    };
                    self.browsing = Some((at, draft));
                    self.show(self.history[at].chars().collect());
                }
            }
            Key::Down => {
                if let Some((at, draft)) = self.browsing.take() {
                    if at + 1 < self.history.len() {
                        self.browsing = Some((at + 1, draft));
                        self.show(self.history[at + 1].chars().collect());
                    } else {
                        self.show(draft);
                    }
                }
            }
            Key::Ignored => {}
        }
        Outcome::Editing
    }
}

/// Columns a character takes in a terminal: 2 for East Asian wide and most emoji, 0 for
/// combining marks, else 1. An approximation of Unicode's East Asian Width.
#[cfg_attr(not(unix), allow(dead_code))] // Windows reads plain lines for now.
fn width(c: char) -> usize {
    match c as u32 {
        0x0300..=0x036f | 0x200b..=0x200f | 0xfe00..=0xfe0f => 0,
        0x1100..=0x115f
        | 0x2e80..=0x303e
        | 0x3041..=0x33ff
        | 0x3400..=0x4dbf
        | 0x4e00..=0x9fff
        | 0xa000..=0xa4cf
        | 0xac00..=0xd7a3
        | 0xf900..=0xfaff
        | 0xfe30..=0xfe4f
        | 0xff00..=0xff60
        | 0xffe0..=0xffe6
        | 0x1f300..=0x1f64f
        | 0x1f900..=0x1f9ff
        | 0x20000..=0x3fffd => 2,
        _ => 1,
    }
}

/// Reads lines with editing on a terminal, plainly otherwise.
#[derive(Default)]
pub struct LineEditor {
    #[cfg_attr(not(unix), allow(dead_code))]
    line: Line,
}

impl LineEditor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads one line after showing `prompt` (on stderr). `Ok(None)` at the end of input
    /// (Ctrl-D on an empty line, or the input closing); Ctrl-C gives an empty line.
    pub fn read_line(&mut self, prompt: &str) -> io::Result<Option<String>> {
        #[cfg(unix)]
        if let Some(raw) = unix::Raw::enter()? {
            let result = self.edit(prompt, &raw);
            drop(raw);
            if let Ok(Some(line)) = &result {
                self.line.remember(line);
            }
            return result;
        }
        let mut err = io::stderr();
        write!(err, "{prompt}")?;
        err.flush()?;
        let mut text = String::new();
        if io::stdin().read_line(&mut text)? == 0 {
            return Ok(None);
        }
        Ok(Some(text.trim_end_matches(['\n', '\r']).to_owned()))
    }

    #[cfg(unix)]
    fn edit(&mut self, prompt: &str, raw: &unix::Raw) -> io::Result<Option<String>> {
        let mut err = io::stderr();
        let mut keys = Keys::default();
        let mut screen = Screen::default();
        self.line.clear();
        draw(&mut err, prompt, &self.line, unix::columns(), &mut screen)?;
        loop {
            let bytes = raw.read()?;
            if bytes.is_empty() {
                // Input closed.
                writeln!(err)?;
                return Ok(None);
            }
            let mut decoded = keys.push(&bytes);
            if decoded.is_empty() && !raw.more_waiting()? {
                decoded = keys.flush();
            }
            for key in decoded {
                let outcome = self.line.apply(key);
                if outcome != Outcome::Editing {
                    // Below the whole line, wherever the cursor was in it.
                    screen.move_to_end(&mut err)?;
                }
                match outcome {
                    Outcome::Editing => {
                        draw(&mut err, prompt, &self.line, unix::columns(), &mut screen)?;
                    }
                    Outcome::Submit => {
                        write!(err, "\r\n")?;
                        err.flush()?;
                        return Ok(Some(self.line.text()));
                    }
                    Outcome::Interrupt => {
                        write!(err, "^C\r\n")?;
                        err.flush()?;
                        return Ok(Some(String::new()));
                    }
                    Outcome::EndOfInput => {
                        write!(err, "\r\n")?;
                        err.flush()?;
                        return Ok(None);
                    }
                }
            }
        }
    }
}

/// Where the last drawing left the cursor, in rows below the line's first row, and how
/// many rows below it the line ends: a line longer than the terminal is wide wraps.
#[derive(Default)]
#[cfg_attr(not(unix), allow(dead_code))]
struct Screen {
    cursor_row: usize,
    end_row: usize,
    end_column: usize,
}

#[cfg_attr(not(unix), allow(dead_code))]
impl Screen {
    /// Moves the cursor to the end of the line (before a newline, or `^C`, ends it).
    fn move_to_end(&mut self, out: &mut impl Write) -> io::Result<()> {
        if self.end_row > self.cursor_row {
            write!(out, "\x1b[{}B", self.end_row - self.cursor_row)?;
        }
        write!(out, "\r")?;
        if self.end_column > 0 {
            write!(out, "\x1b[{}C", self.end_column)?;
        }
        self.cursor_row = self.end_row;
        Ok(())
    }
}

/// The row and column after `chars` on a terminal `columns` wide, starting at the top
/// left. A character that does not fit in the rest of a row goes to the next one; a row
/// filled exactly leaves the column at `columns` (the terminal's pending wrap).
#[cfg_attr(not(unix), allow(dead_code))]
fn place(chars: impl Iterator<Item = char>, columns: usize) -> (usize, usize) {
    let (mut row, mut column) = (0, 0);
    for c in chars {
        let w = width(c);
        if w == 0 {
            continue;
        }
        if column + w > columns {
            row += 1;
            column = 0;
        }
        column += w;
    }
    (row, column)
}

/// Redraws the prompt and line from the line's first row, which may be above the cursor
/// when the line wraps, then puts the cursor back in place.
#[cfg_attr(not(unix), allow(dead_code))]
fn draw(
    out: &mut impl Write,
    prompt: &str,
    line: &Line,
    columns: usize,
    screen: &mut Screen,
) -> io::Result<()> {
    let columns = columns.max(2);
    let text: Vec<char> = line.text().chars().collect();
    if screen.cursor_row > 0 {
        write!(out, "\x1b[{}A", screen.cursor_row)?;
    }
    let shown: String = text.iter().collect();
    write!(out, "\r\x1b[J{prompt}{shown}")?;
    let (mut end_row, mut end_column) = place(prompt.chars().chain(text.iter().copied()), columns);
    if end_column == columns {
        // A full last row: start the next one, so the cursor has a defined place.
        write!(out, "\r\n")?;
        end_row += 1;
        end_column = 0;
    }
    let cursor = line.cursor();
    let (mut row, mut column) = place(
        prompt.chars().chain(text[..cursor].iter().copied()),
        columns,
    );
    let next = text.get(cursor).map_or(1, |&c| width(c));
    if column == columns || column + next > columns {
        // The next character starts the following row.
        row += 1;
        column = 0;
    }
    if end_row > row {
        write!(out, "\x1b[{}A", end_row - row)?;
    }
    write!(out, "\r")?;
    if column > 0 {
        write!(out, "\x1b[{column}C")?;
    }
    screen.cursor_row = row;
    screen.end_row = end_row;
    screen.end_column = end_column;
    out.flush()
}

#[cfg(unix)]
mod unix {
    use std::io;

    /// The terminal in raw mode for the life of the value: no echo, no line buffering,
    /// no signals from Ctrl-C (the editor handles it); output processing kept.
    pub struct Raw {
        saved: libc::termios,
    }

    impl Raw {
        /// `None` when stdin is not a terminal.
        pub fn enter() -> io::Result<Option<Self>> {
            // SAFETY: isatty and tcgetattr only read the descriptor's state into `saved`.
            unsafe {
                if libc::isatty(libc::STDIN_FILENO) != 1 {
                    return Ok(None);
                }
                let mut saved: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(libc::STDIN_FILENO, &mut saved) != 0 {
                    return Err(io::Error::last_os_error());
                }
                let mut raw = saved;
                raw.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG | libc::IEXTEN);
                raw.c_iflag &= !(libc::IXON | libc::ICRNL | libc::INLCR);
                raw.c_cc[libc::VMIN] = 1;
                raw.c_cc[libc::VTIME] = 0;
                // TCSANOW: TCSAFLUSH would throw away keys typed ahead (while a reply was
                // still printing, or piped in).
                if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Some(Self { saved }))
            }
        }

        /// The bytes available, waiting for at least one; empty at the end of input.
        pub fn read(&self) -> io::Result<Vec<u8>> {
            let mut buffer = [0_u8; 256];
            loop {
                // SAFETY: reads into a buffer of the length given.
                let n = unsafe {
                    libc::read(libc::STDIN_FILENO, buffer.as_mut_ptr().cast(), buffer.len())
                };
                if n >= 0 {
                    return Ok(buffer[..n as usize].to_vec());
                }
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }

        /// Whether more input arrives within 50 ms: tells a lone Escape from the start of
        /// a sequence whose rest is still in flight.
        pub fn more_waiting(&self) -> io::Result<bool> {
            let mut poll = libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one pollfd, as the count says.
            let n = unsafe { libc::poll(&mut poll, 1, 50) };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(n > 0)
        }
    }

    /// The terminal's width in columns (80 when it cannot be read).
    pub fn columns() -> usize {
        // SAFETY: TIOCGWINSZ writes one winsize into `size`.
        unsafe {
            let mut size: libc::winsize = std::mem::zeroed();
            if libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &mut size) == 0 && size.ws_col > 0
            {
                return usize::from(size.ws_col);
            }
        }
        80
    }

    impl Drop for Raw {
        fn drop(&mut self) {
            // SAFETY: restores the settings read in `enter`.
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.saved);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(line: &mut Line, bytes: &[u8]) -> Vec<Outcome> {
        let mut keys = Keys::default();
        keys.push(bytes)
            .into_iter()
            .map(|k| line.apply(k))
            .collect()
    }

    #[test]
    fn keys_decode_arrows_words_and_split_sequences() {
        let mut keys = Keys::default();
        assert_eq!(
            keys.push(b"a\x1b[D\x1b[C\x1bOH\x1b[F"),
            [Key::Char('a'), Key::Left, Key::Right, Key::Home, Key::End]
        );
        assert_eq!(
            keys.push(b"\x1b[1;5D\x1bb\x1b[3~\x7f"),
            [Key::WordLeft, Key::WordLeft, Key::Delete, Key::Backspace]
        );
        // A sequence and a multi-byte character split across reads.
        assert_eq!(keys.push(b"\x1b["), []);
        assert_eq!(keys.push(b"A\xe4"), [Key::Up]);
        assert_eq!(keys.push(b"\xb8\xad"), [Key::Char('中')]);
        // A lone Escape ends when nothing follows.
        assert_eq!(keys.push(b"\x1b"), []);
        assert_eq!(keys.flush(), [Key::Ignored]);
        assert_eq!(keys.push(b"\x1b[200~"), [Key::Ignored]);
    }

    #[test]
    fn editing_moves_inserts_and_deletes_by_character_and_word() {
        let mut line = Line::default();
        typed(&mut line, b"hello world");
        // Left twice, insert, Home, Delete, End, Ctrl-W.
        typed(&mut line, b"\x1b[D\x1b[DX\x01\x1b[3~");
        assert_eq!(line.text(), "ello worXld");
        typed(&mut line, b"\x05\x17");
        assert_eq!(line.text(), "ello ");
        typed(&mut line, b"one two\x1b[1;5D\x0b");
        assert_eq!(line.text(), "ello one ");
        typed(&mut line, b"\x1bb\x15");
        assert_eq!((line.text().as_str(), line.cursor()), ("one ", 0));
        // Multi-byte characters move as one.
        let mut line = Line::default();
        typed(&mut line, "中文ok".as_bytes());
        typed(&mut line, b"\x1b[D\x1b[D\x1b[D\x7f");
        assert_eq!(line.text(), "文ok");
    }

    #[test]
    fn history_steps_back_and_forward_and_keeps_the_draft() {
        let mut line = Line::default();
        line.remember("first");
        line.remember("second");
        line.remember("second");
        line.remember("  ");
        typed(&mut line, b"draft");
        typed(&mut line, b"\x1b[A");
        assert_eq!(line.text(), "second");
        typed(&mut line, b"\x1b[A\x1b[A");
        assert_eq!(line.text(), "first");
        typed(&mut line, b"\x1b[B");
        assert_eq!(line.text(), "second");
        typed(&mut line, b"\x1b[B");
        assert_eq!((line.text().as_str(), line.cursor()), ("draft", 5));
    }

    #[test]
    fn enter_ctrl_c_and_ctrl_d_end_the_line() {
        let mut line = Line::default();
        assert_eq!(typed(&mut line, b"\x04"), [Outcome::EndOfInput]);
        assert_eq!(
            typed(&mut line, b"ab\x01\x04"),
            [
                Outcome::Editing,
                Outcome::Editing,
                Outcome::Editing,
                Outcome::Editing
            ]
        );
        assert_eq!(line.text(), "b");
        assert_eq!(typed(&mut line, b"\x03"), [Outcome::Interrupt]);
        assert_eq!(typed(&mut line, b"\r"), [Outcome::Submit]);
    }

    #[test]
    fn drawing_puts_the_cursor_back_by_display_columns() {
        let mut line = Line::default();
        typed(&mut line, "a中b".as_bytes());
        typed(&mut line, b"\x1b[D\x1b[D");
        let mut out = Vec::new();
        draw(&mut out, "> ", &line, 80, &mut Screen::default()).unwrap();
        // The cursor goes before 中: column 3.
        assert_eq!(String::from_utf8(out).unwrap(), "\r\x1b[J> a中b\r\x1b[3C");
    }

    #[test]
    fn a_wrapped_line_is_redrawn_from_its_first_row() {
        // 10 columns: "> " and 15 characters take two rows.
        let mut line = Line::default();
        typed(&mut line, b"abcdefghijklmno");
        let mut screen = Screen::default();
        let mut out = Vec::new();
        draw(&mut out, "> ", &line, 10, &mut screen).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\r\x1b[J> abcdefghijklmno\r\x1b[7C"
        );
        assert_eq!((screen.cursor_row, screen.end_row), (1, 1));
        // Home: the next drawing goes up a row first, and leaves the cursor on the first.
        typed(&mut line, b"\x01");
        let mut out = Vec::new();
        draw(&mut out, "> ", &line, 10, &mut screen).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b[1A\r\x1b[J> abcdefghijklmno\x1b[1A\r\x1b[2C"
        );
        assert_eq!((screen.cursor_row, screen.end_row), (0, 1));
        // Enter from there moves below the line before the newline.
        let mut out = Vec::new();
        screen.move_to_end(&mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "\x1b[1B\r\x1b[7C");
    }

    #[test]
    fn full_rows_and_wide_characters_wrap_where_the_terminal_wraps_them() {
        // Exactly one full row: the cursor starts the next row.
        let mut line = Line::default();
        typed(&mut line, b"abcdefgh");
        let mut screen = Screen::default();
        let mut out = Vec::new();
        draw(&mut out, "> ", &line, 10, &mut screen).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "\r\x1b[J> abcdefgh\r\n\r");
        assert_eq!((screen.cursor_row, screen.end_row), (1, 1));
        // A wide character that does not fit in the last column goes to the next row.
        assert_eq!(place("> abcdefg中".chars(), 10), (1, 2));
        assert_eq!(place("> abcdef中".chars(), 10), (0, 10));
    }
}
