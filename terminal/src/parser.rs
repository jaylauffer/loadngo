//! The escape-sequence parser: the state machine of DEC's VT500-series
//! terminals (as charted at vt100.net/emu/dec_ansi_parser), over UTF-8
//! text. It only splits the byte stream into actions; [`Perform`] decides
//! what they mean.

/// What the parser found.
pub trait Perform {
    /// A character to show.
    fn print(&mut self, ch: char);
    /// A C0 control (BEL, BS, HT, LF, CR, ...).
    fn execute(&mut self, byte: u8);
    /// A control sequence: `ESC [`, an optional private marker (`?`, `>`,
    /// `<`, `=`), parameters, intermediates and the final character.
    fn csi(&mut self, params: &Params, private: Option<u8>, intermediates: &[u8], action: char);
    /// An escape sequence: `ESC`, intermediates and the final byte.
    fn esc(&mut self, intermediates: &[u8], byte: u8);
    /// An operating system command (`ESC ]` ... BEL or ST), without its
    /// terminator.
    fn osc(&mut self, data: &[u8]);
}

/// The most parameters kept; later ones are dropped.
const MAX_PARAMS: usize = 32;
/// The longest OSC kept; the rest is dropped.
const MAX_OSC: usize = 4096;

/// A control sequence's numeric parameters. An empty parameter reads as
/// zero. Parameters separated by `:` (as in `38:2::r:g:b`) are marked, so a
/// caller can read them as one group.
#[derive(Debug, Clone, Default)]
pub struct Params {
    values: [u16; MAX_PARAMS],
    len: usize,
    /// Bit `i` set: parameter `i` is followed by `:`, not `;`.
    colon_after: u32,
}

impl Params {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Parameter `index`, zero when absent.
    pub fn get(&self, index: usize) -> u16 {
        if index < self.len {
            self.values[index]
        } else {
            0
        }
    }

    /// Parameter `index`, `default` when absent or zero (the usual rule for
    /// counts and positions).
    pub fn get_or(&self, index: usize, default: u16) -> u16 {
        match self.get(index) {
            0 => default,
            value => value,
        }
    }

    /// Whether parameter `index` is followed by a `:` subparameter.
    pub fn has_sub(&self, index: usize) -> bool {
        index < self.len && self.colon_after & (1 << index) != 0
    }

    pub fn iter(&self) -> impl Iterator<Item = u16> + '_ {
        self.values[..self.len].iter().copied()
    }

    fn clear(&mut self) {
        self.len = 0;
        self.colon_after = 0;
    }

    fn push(&mut self, value: u16, colon: bool) {
        if self.len < MAX_PARAMS {
            if colon {
                self.colon_after |= 1 << self.len;
            }
            self.values[self.len] = value;
            self.len += 1;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    Osc,
    /// DCS, SOS, PM and APC strings: read to their terminator and dropped.
    IgnoredString,
}

#[derive(Debug, Clone)]
pub struct Parser {
    state: State,
    params: Params,
    /// The parameter being read, if any digit has come.
    current: Option<u32>,
    private: Option<u8>,
    intermediates: [u8; 2],
    intermediate_len: usize,
    osc: Vec<u8>,
    /// An ESC arrived inside a string: a `\` next ends it (ST).
    string_escape: bool,
    utf8: [u8; 4],
    utf8_len: usize,
    utf8_need: usize,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            params: Params::default(),
            current: None,
            private: None,
            intermediates: [0; 2],
            intermediate_len: 0,
            osc: Vec::new(),
            string_escape: false,
            utf8: [0; 4],
            utf8_len: 0,
            utf8_need: 0,
        }
    }

    pub fn advance<P: Perform>(&mut self, performer: &mut P, bytes: &[u8]) {
        for &byte in bytes {
            self.byte(performer, byte);
        }
    }

    fn clear_sequence(&mut self) {
        self.params.clear();
        self.current = None;
        self.private = None;
        self.intermediate_len = 0;
    }

    fn collect(&mut self, byte: u8) {
        if self.intermediate_len < self.intermediates.len() {
            self.intermediates[self.intermediate_len] = byte;
            self.intermediate_len += 1;
        }
    }

    fn finish_param(&mut self, colon: bool) {
        let value = self.current.take().unwrap_or(0).min(u32::from(u16::MAX)) as u16;
        self.params.push(value, colon);
    }

    fn byte<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        if self.state == State::Ground && self.utf8_need > 0 {
            self.utf8_continue(performer, byte);
            return;
        }
        // Inside a string, ESC may start its terminator.
        if matches!(self.state, State::Osc | State::IgnoredString) {
            if self.string_escape {
                self.string_escape = false;
                self.end_string(performer);
                if byte == b'\\' {
                    return;
                }
                // ESC and something else: the string ended, and an escape
                // sequence began.
                self.clear_sequence();
                self.state = State::Escape;
                self.byte(performer, byte);
                return;
            }
            match byte {
                0x1b => self.string_escape = true,
                0x07 => self.end_string(performer),
                0x18 | 0x1a => self.state = State::Ground,
                _ if self.state == State::Osc && byte >= 0x20 && self.osc.len() < MAX_OSC => {
                    self.osc.push(byte);
                }
                _ => {}
            }
            return;
        }
        match byte {
            0x18 | 0x1a => {
                self.state = State::Ground;
                return;
            }
            0x1b => {
                self.clear_sequence();
                self.state = State::Escape;
                return;
            }
            _ => {}
        }
        match self.state {
            State::Ground => match byte {
                0x00..=0x1f => performer.execute(byte),
                0x20..=0x7e => performer.print(byte as char),
                0x7f => {}
                _ => self.utf8_start(performer, byte),
            },
            State::Escape => match byte {
                0x00..=0x1f => performer.execute(byte),
                0x20..=0x2f => {
                    self.collect(byte);
                    self.state = State::EscapeIntermediate;
                }
                b'[' => self.state = State::CsiEntry,
                b']' => {
                    self.osc.clear();
                    self.state = State::Osc;
                }
                b'P' | b'X' | b'^' | b'_' => self.state = State::IgnoredString,
                0x30..=0x7e => {
                    performer.esc(&self.intermediates[..self.intermediate_len], byte);
                    self.state = State::Ground;
                }
                _ => {}
            },
            State::EscapeIntermediate => match byte {
                0x00..=0x1f => performer.execute(byte),
                0x20..=0x2f => self.collect(byte),
                0x30..=0x7e => {
                    performer.esc(&self.intermediates[..self.intermediate_len], byte);
                    self.state = State::Ground;
                }
                _ => {}
            },
            State::CsiEntry | State::CsiParam => match byte {
                0x00..=0x1f => performer.execute(byte),
                b'0'..=b'9' => {
                    let digit = u32::from(byte - b'0');
                    self.current = Some(
                        self.current
                            .unwrap_or(0)
                            .saturating_mul(10)
                            .saturating_add(digit),
                    );
                    self.state = State::CsiParam;
                }
                b';' | b':' => {
                    self.finish_param(byte == b':');
                    self.state = State::CsiParam;
                }
                0x3c..=0x3f => {
                    if self.state == State::CsiEntry {
                        self.private = Some(byte);
                        self.state = State::CsiParam;
                    } else {
                        self.state = State::CsiIgnore;
                    }
                }
                0x20..=0x2f => {
                    if self.current.is_some() || !self.params.is_empty() {
                        self.finish_param(false);
                    }
                    self.collect(byte);
                    self.state = State::CsiIntermediate;
                }
                0x40..=0x7e => {
                    if self.current.is_some() || !self.params.is_empty() {
                        self.finish_param(false);
                    }
                    self.dispatch_csi(performer, byte);
                }
                _ => {}
            },
            State::CsiIntermediate => match byte {
                0x00..=0x1f => performer.execute(byte),
                0x20..=0x2f => self.collect(byte),
                0x30..=0x3f => self.state = State::CsiIgnore,
                0x40..=0x7e => self.dispatch_csi(performer, byte),
                _ => {}
            },
            State::CsiIgnore => match byte {
                0x00..=0x1f => performer.execute(byte),
                0x40..=0x7e => self.state = State::Ground,
                _ => {}
            },
            State::Osc | State::IgnoredString => unreachable!("handled above"),
        }
    }

    fn dispatch_csi<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        performer.csi(
            &self.params,
            self.private,
            &self.intermediates[..self.intermediate_len],
            byte as char,
        );
        self.state = State::Ground;
    }

    fn end_string<P: Perform>(&mut self, performer: &mut P) {
        if self.state == State::Osc {
            performer.osc(&self.osc);
        }
        self.state = State::Ground;
    }

    fn utf8_start<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        let need = match byte {
            0xc2..=0xdf => 1,
            0xe0..=0xef => 2,
            0xf0..=0xf4 => 3,
            _ => {
                performer.print(char::REPLACEMENT_CHARACTER);
                return;
            }
        };
        self.utf8[0] = byte;
        self.utf8_len = 1;
        self.utf8_need = need;
    }

    fn utf8_continue<P: Perform>(&mut self, performer: &mut P, byte: u8) {
        if byte & 0xc0 != 0x80 {
            // Not a continuation: the sequence was cut short.
            self.utf8_need = 0;
            performer.print(char::REPLACEMENT_CHARACTER);
            self.byte(performer, byte);
            return;
        }
        self.utf8[self.utf8_len] = byte;
        self.utf8_len += 1;
        self.utf8_need -= 1;
        if self.utf8_need == 0 {
            let ch = std::str::from_utf8(&self.utf8[..self.utf8_len])
                .ok()
                .and_then(|text| text.chars().next())
                .unwrap_or(char::REPLACEMENT_CHARACTER);
            performer.print(ch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Log(Vec<String>);

    impl Perform for Log {
        fn print(&mut self, ch: char) {
            self.0.push(format!("print {ch}"));
        }
        fn execute(&mut self, byte: u8) {
            self.0.push(format!("exec {byte:#x}"));
        }
        fn csi(
            &mut self,
            params: &Params,
            private: Option<u8>,
            intermediates: &[u8],
            action: char,
        ) {
            let values: Vec<String> = (0..params.len())
                .map(|i| {
                    format!(
                        "{}{}",
                        params.get(i),
                        if params.has_sub(i) { ":" } else { "" }
                    )
                })
                .collect();
            self.0.push(format!(
                "csi {}{}{} {action}",
                private
                    .map(|p| p as char)
                    .map(String::from)
                    .unwrap_or_default(),
                values.join(","),
                String::from_utf8_lossy(intermediates),
            ));
        }
        fn esc(&mut self, intermediates: &[u8], byte: u8) {
            self.0.push(format!(
                "esc {}{}",
                String::from_utf8_lossy(intermediates),
                byte as char
            ));
        }
        fn osc(&mut self, data: &[u8]) {
            self.0
                .push(format!("osc {}", String::from_utf8_lossy(data)));
        }
    }

    fn parse(bytes: &[u8]) -> Vec<String> {
        let mut log = Log::default();
        let mut parser = Parser::new();
        parser.advance(&mut log, bytes);
        log.0
    }

    #[test]
    fn splits_text_controls_and_sequences() {
        assert_eq!(
            parse(b"a\r\n\x1b[1;31mb\x1b[?25l\x1b[2 q\x1b(0"),
            [
                "print a",
                "exec 0xd",
                "exec 0xa",
                "csi 1,31 m",
                "print b",
                "csi ?25 l",
                "csi 2  q",
                "esc (0",
            ]
        );
    }

    #[test]
    fn empty_and_colon_parameters() {
        assert_eq!(
            parse(b"\x1b[;5H\x1b[38:2::1:2:3m\x1b[m"),
            ["csi 0,5 H", "csi 38:,2:,0:,1:,2:,3 m", "csi  m"]
        );
    }

    #[test]
    fn osc_ends_with_bel_or_st_and_dcs_is_dropped() {
        assert_eq!(
            parse(b"\x1b]0;title\x07\x1b]2;two\x1b\\\x1bPq#0;1\x1b\\x"),
            ["osc 0;title", "osc 2;two", "print x"]
        );
    }

    #[test]
    fn utf8_across_chunks_and_bad_bytes() {
        let mut log = Log::default();
        let mut parser = Parser::new();
        let text = "é😀".as_bytes();
        for chunk in text.chunks(1) {
            parser.advance(&mut log, chunk);
        }
        parser.advance(&mut log, b"\xffz\xe2\x82");
        parser.advance(&mut log, b"y");
        assert_eq!(
            log.0,
            [
                "print é",
                "print 😀",
                "print \u{fffd}",
                "print z",
                "print \u{fffd}",
                "print y"
            ]
        );
    }
}
