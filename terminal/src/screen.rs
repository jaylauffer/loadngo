//! The screen: a grid of cells with scrollback, a cursor and the modes an
//! xterm-compatible terminal keeps, driven by [`Parser`](crate::Parser).
//!
//! It implements what shells, `cargo`, `git`, `less`, `vim`, `top` and the
//! like use: cursor movement, erasing, insert and delete, scroll regions,
//! the alternate screen, 16, 256 and 24-bit colors, line drawing, bracketed
//! paste and the replies programs ask for (cursor position, device
//! attributes, colors). Not implemented: mouse reporting, double-width
//! lines, reflow on resize, and combining characters (they are dropped).

use std::collections::VecDeque;
use std::path::PathBuf;

use crate::parser::{Params, Parser, Perform};
use crate::width::char_width;

/// A color as the program asked for it; the view picks the actual colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Default,
    /// The 256-color palette: 0-7 normal, 8-15 bright, then the 6x6x6 cube
    /// and the grays.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

pub const BOLD: u8 = 1;
pub const DIM: u8 = 2;
pub const ITALIC: u8 = 4;
pub const UNDERLINE: u8 = 8;
pub const INVERSE: u8 = 16;
pub const HIDDEN: u8 = 32;
pub const STRIKE: u8 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    /// [`BOLD`], [`DIM`], [`ITALIC`], [`UNDERLINE`], [`INVERSE`],
    /// [`HIDDEN`], [`STRIKE`].
    pub flags: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub style: Style,
    /// 1 for most characters; 2 for the first cell of a wide one, 0 for
    /// the cell it covers.
    pub width: u8,
}

impl Cell {
    pub const BLANK: Cell = Cell {
        ch: ' ',
        style: Style {
            fg: Color::Default,
            bg: Color::Default,
            flags: 0,
        },
        width: 1,
    };

    fn blank(bg: Color) -> Cell {
        Cell {
            style: Style {
                bg,
                ..Style::default()
            },
            ..Cell::BLANK
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub cells: Vec<Cell>,
    /// The line continues on the next one (it wrapped, no newline).
    pub wrapped: bool,
}

impl Line {
    fn new(cols: usize) -> Self {
        Self {
            cells: vec![Cell::BLANK; cols],
            wrapped: false,
        }
    }

    fn reset(&mut self, cols: usize, blank: Cell) {
        self.cells.clear();
        self.cells.resize(cols, blank);
        self.wrapped = false;
    }

    fn is_blank(&self) -> bool {
        self.cells.iter().all(|cell| *cell == Cell::BLANK)
    }

    /// The line's text, trailing spaces trimmed.
    pub fn text(&self) -> String {
        let mut text: String = self
            .cells
            .iter()
            .filter(|cell| cell.width != 0)
            .map(|cell| cell.ch)
            .collect();
        text.truncate(text.trim_end().len());
        text
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Cursor {
    row: usize,
    col: usize,
    style: Style,
    /// The last column was written: the next character wraps first.
    pending_wrap: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct SavedCursor {
    cursor: Cursor,
    origin: bool,
    charsets: [Charset; 2],
    active_charset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Charset {
    #[default]
    Ascii,
    /// DEC special graphics: line drawing.
    Graphics,
}

/// Modes a program can switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Modes {
    /// Cursor keys send `ESC O x` rather than `ESC [ x` (DECCKM).
    pub app_cursor: bool,
    pub autowrap: bool,
    pub show_cursor: bool,
    /// Pasted text is wrapped in `ESC [200~` ... `ESC [201~`.
    pub bracketed_paste: bool,
    insert: bool,
    origin: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            app_cursor: false,
            autowrap: true,
            show_cursor: true,
            bracketed_paste: false,
            insert: false,
            origin: false,
        }
    }
}

/// Lines kept above the screen.
pub const SCROLLBACK_LINES: usize = 5_000;

pub struct Terminal {
    cols: usize,
    rows: usize,
    /// The normal screen: scrollback, then the `rows` lines on screen.
    primary: VecDeque<Line>,
    /// The alternate screen (full-screen programs): no scrollback.
    alternate: Vec<Line>,
    alt_active: bool,
    cursor: Cursor,
    /// Saved cursors of the normal and the alternate screen.
    saved: [SavedCursor; 2],
    scroll_top: usize,
    scroll_bottom: usize,
    pub modes: Modes,
    tabs: Vec<bool>,
    charsets: [Charset; 2],
    active_charset: usize,
    last_printed: Option<char>,
    parser: Parser,
    /// The window title a program set (OSC 0 or 2).
    pub title: String,
    /// The working directory the shell reported (OSC 7).
    pub cwd: Option<PathBuf>,
    /// Replies to programs' questions, to send to the program.
    responses: Vec<u8>,
    /// The bell rang since the last [`Terminal::take_bell`].
    bell: bool,
    /// Something on screen changed since the last [`Terminal::take_changed`].
    changed: bool,
    /// Lines that scrolled into the scrollback since the last
    /// [`Terminal::take_scrolled`], so a view scrolled back can stay put.
    scrolled: usize,
    /// What color the view paints the default background and foreground,
    /// for programs that ask (OSC 10/11).
    pub default_colors: [(u8, u8, u8); 2],
}

impl Terminal {
    pub fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(2);
        let rows = rows.max(1);
        Self {
            cols,
            rows,
            primary: (0..rows).map(|_| Line::new(cols)).collect(),
            alternate: (0..rows).map(|_| Line::new(cols)).collect(),
            alt_active: false,
            cursor: Cursor::default(),
            saved: [SavedCursor::default(); 2],
            scroll_top: 0,
            scroll_bottom: rows - 1,
            modes: Modes::default(),
            tabs: default_tabs(cols),
            charsets: [Charset::Ascii; 2],
            active_charset: 0,
            last_printed: None,
            parser: Parser::new(),
            title: String::new(),
            cwd: None,
            responses: Vec::new(),
            bell: false,
            changed: true,
            scrolled: 0,
            default_colors: [(0xdc, 0xe2, 0xec), (0x14, 0x19, 0x21)],
        }
    }

    /// Feeds output from the program.
    pub fn feed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut parser = std::mem::take(&mut self.parser);
        parser.advance(self, bytes);
        self.parser = parser;
        self.changed = true;
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Whether a full-screen program has the alternate screen.
    pub fn alternate_screen(&self) -> bool {
        self.alt_active
    }

    /// The cursor's row and column on screen, when it shows.
    pub fn cursor(&self) -> Option<(usize, usize)> {
        self.modes
            .show_cursor
            .then_some((self.cursor.row, self.cursor.col.min(self.cols - 1)))
    }

    /// Lines in the scrollback (none on the alternate screen).
    pub fn scrollback_len(&self) -> usize {
        if self.alt_active {
            0
        } else {
            self.primary.len() - self.rows
        }
    }

    /// Line `index` counting from the oldest scrollback line; the screen's
    /// first row is `scrollback_len()`.
    pub fn line(&self, index: usize) -> Option<&Line> {
        if self.alt_active {
            self.alternate.get(index)
        } else {
            self.primary.get(index)
        }
    }

    /// Replies owed to the program (cursor position reports and the like).
    pub fn take_responses(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.responses)
    }

    pub fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.bell)
    }

    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    pub fn take_scrolled(&mut self) -> usize {
        std::mem::take(&mut self.scrolled)
    }

    /// Changes the size. Lines are cut or padded, not rewrapped; a smaller
    /// screen first drops blank lines below the cursor, then moves lines
    /// into the scrollback, and a taller one brings them back.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(2);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        for line in self.primary.iter_mut().chain(self.alternate.iter_mut()) {
            line.cells.resize(cols, Cell::BLANK);
            if let Some(last) = line.cells.last_mut() {
                if last.width == 2 {
                    *last = Cell::BLANK;
                }
            }
        }
        // The normal screen.
        let start = self.primary.len() - self.rows;
        let mut cursor_line = start
            + if self.alt_active {
                self.saved[0].cursor.row
            } else {
                self.cursor.row
            };
        if rows < self.rows {
            let mut drop = self.rows - rows;
            while drop > 0
                && self.primary.len() - 1 > cursor_line
                && self.primary.back().is_some_and(Line::is_blank)
            {
                self.primary.pop_back();
                drop -= 1;
            }
        } else {
            while self.primary.len() < rows {
                self.primary.push_back(Line::new(cols));
            }
        }
        let new_start = self.primary.len() - rows;
        cursor_line = cursor_line.max(new_start);
        let primary_row = cursor_line - new_start;
        // The alternate screen keeps its top lines.
        self.alternate.resize_with(rows, || Line::new(cols));
        let alternate_row = if self.alt_active {
            self.cursor.row
        } else {
            self.saved[1].cursor.row
        };
        if self.alt_active {
            self.cursor.row = alternate_row.min(rows - 1);
            self.saved[0].cursor.row = primary_row.min(rows - 1);
        } else {
            self.cursor.row = primary_row.min(rows - 1);
            self.saved[1].cursor.row = alternate_row.min(rows - 1);
        }
        self.cols = cols;
        self.rows = rows;
        self.cursor.col = self.cursor.col.min(cols - 1);
        self.cursor.pending_wrap = false;
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.tabs = default_tabs(cols);
        self.changed = true;
    }

    // ----- the grid -----

    fn screen_line(&mut self, row: usize) -> &mut Line {
        if self.alt_active {
            &mut self.alternate[row]
        } else {
            let start = self.primary.len() - self.rows;
            &mut self.primary[start + row]
        }
    }

    fn blank_cell(&self) -> Cell {
        Cell::blank(self.cursor.style.bg)
    }

    /// Scrolls the region up `count` lines. Lines leaving the top of a
    /// full-screen region of the normal screen go to the scrollback.
    fn scroll_up(&mut self, count: usize) {
        let count = count.min(self.scroll_bottom - self.scroll_top + 1);
        let blank = self.blank_cell();
        let cols = self.cols;
        if !self.alt_active && self.scroll_top == 0 && self.scroll_bottom == self.rows - 1 {
            for _ in 0..count {
                let mut line = if self.primary.len() - self.rows >= SCROLLBACK_LINES {
                    self.primary.pop_front().unwrap_or_else(|| Line::new(cols))
                } else {
                    Line::new(cols)
                };
                line.reset(cols, blank);
                self.primary.push_back(line);
                self.scrolled += 1;
            }
            return;
        }
        for _ in 0..count {
            let top = self.scroll_top;
            let bottom = self.scroll_bottom;
            if self.alt_active {
                let mut line = self.alternate.remove(top);
                line.reset(cols, blank);
                self.alternate.insert(bottom, line);
            } else {
                let start = self.primary.len() - self.rows;
                let mut line = self.primary.remove(start + top).expect("on screen");
                line.reset(cols, blank);
                self.primary.insert(start + bottom, line);
            }
        }
    }

    fn scroll_down(&mut self, count: usize) {
        let count = count.min(self.scroll_bottom - self.scroll_top + 1);
        let blank = self.blank_cell();
        let cols = self.cols;
        for _ in 0..count {
            let top = self.scroll_top;
            let bottom = self.scroll_bottom;
            if self.alt_active {
                let mut line = self.alternate.remove(bottom);
                line.reset(cols, blank);
                self.alternate.insert(top, line);
            } else {
                let start = self.primary.len() - self.rows;
                let mut line = self.primary.remove(start + bottom).expect("on screen");
                line.reset(cols, blank);
                self.primary.insert(start + top, line);
            }
        }
    }

    fn line_feed(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.cursor.row + 1 < self.rows {
            self.cursor.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.scroll_top {
            self.scroll_down(1);
        } else if self.cursor.row > 0 {
            self.cursor.row -= 1;
        }
    }

    fn move_to(&mut self, row: usize, col: usize) {
        let (top, bottom) = if self.modes.origin {
            (self.scroll_top, self.scroll_bottom)
        } else {
            (0, self.rows - 1)
        };
        self.cursor.row = (top + row).min(bottom);
        self.cursor.col = col.min(self.cols - 1);
        self.cursor.pending_wrap = false;
    }

    /// Clears cells `from..to` of a screen row, mending wide characters cut
    /// in half.
    fn erase_cells(&mut self, row: usize, from: usize, to: usize) {
        let blank = self.blank_cell();
        let cols = self.cols;
        let line = self.screen_line(row);
        let to = to.min(cols);
        if from >= to {
            return;
        }
        if from > 0 && line.cells[from].width == 0 {
            line.cells[from - 1] = blank;
        }
        if to < cols && line.cells[to].width == 0 {
            line.cells[to] = blank;
        }
        for cell in &mut line.cells[from..to] {
            *cell = blank;
        }
    }

    fn erase_rows(&mut self, from: usize, to: usize) {
        for row in from..to.min(self.rows) {
            self.erase_cells(row, 0, self.cols);
            self.screen_line(row).wrapped = false;
        }
    }

    fn print_char(&mut self, ch: char) {
        let ch = match self.charsets[self.active_charset] {
            Charset::Graphics => dec_graphics(ch),
            Charset::Ascii => ch,
        };
        let width = char_width(ch);
        if width == 0 {
            return;
        }
        if self.cursor.pending_wrap && self.modes.autowrap {
            self.screen_line(self.cursor.row).wrapped = true;
            self.cursor.col = 0;
            self.line_feed();
        }
        if width == 2 && self.cursor.col + 1 >= self.cols {
            // A wide character does not fit in the last column.
            if self.modes.autowrap {
                let row = self.cursor.row;
                self.erase_cells(row, self.cursor.col, self.cols);
                self.screen_line(row).wrapped = true;
                self.cursor.col = 0;
                self.line_feed();
            } else {
                return;
            }
        }
        let row = self.cursor.row;
        let col = self.cursor.col;
        let cols = self.cols;
        let blank = self.blank_cell();
        let style = self.cursor.style;
        if self.modes.insert {
            let line = self.screen_line(row);
            for _ in 0..width {
                line.cells.insert(col, blank);
                line.cells.truncate(cols);
            }
        }
        // Overwriting half of a wide character clears its other half.
        self.erase_cells(row, col, col + usize::from(width));
        let line = self.screen_line(row);
        line.cells[col] = Cell { ch, style, width };
        if width == 2 {
            line.cells[col + 1] = Cell {
                ch: ' ',
                style,
                width: 0,
            };
        }
        let next = col + usize::from(width);
        if next >= cols {
            self.cursor.col = cols - 1;
            self.cursor.pending_wrap = self.modes.autowrap;
        } else {
            self.cursor.col = next;
        }
        self.last_printed = Some(ch);
    }

    fn set_alternate(&mut self, on: bool, save_cursor: bool) {
        if on == self.alt_active {
            return;
        }
        if on {
            if save_cursor {
                self.save_cursor();
            }
            self.alt_active = true;
            let blank = Cell::BLANK;
            let cols = self.cols;
            for line in &mut self.alternate {
                line.reset(cols, blank);
            }
        } else {
            self.alt_active = false;
            if save_cursor {
                self.restore_cursor();
            }
        }
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
    }

    fn save_cursor(&mut self) {
        let index = usize::from(self.alt_active);
        self.saved[index] = SavedCursor {
            cursor: self.cursor,
            origin: self.modes.origin,
            charsets: self.charsets,
            active_charset: self.active_charset,
        };
    }

    fn restore_cursor(&mut self) {
        let saved = self.saved[usize::from(self.alt_active)];
        self.cursor = saved.cursor;
        self.cursor.row = self.cursor.row.min(self.rows - 1);
        self.cursor.col = self.cursor.col.min(self.cols - 1);
        self.modes.origin = saved.origin;
        self.charsets = saved.charsets;
        self.active_charset = saved.active_charset;
    }

    fn full_reset(&mut self) {
        let colors = self.default_colors;
        let title = std::mem::take(&mut self.title);
        let cwd = self.cwd.take();
        *self = Terminal::new(self.cols, self.rows);
        self.default_colors = colors;
        self.title = title;
        self.cwd = cwd;
    }

    fn set_mode(&mut self, params: &Params, private: bool, on: bool) {
        for mode in params.iter() {
            match (private, mode) {
                (false, 4) => self.modes.insert = on,
                (true, 1) => self.modes.app_cursor = on,
                (true, 6) => {
                    self.modes.origin = on;
                    self.move_to(0, 0);
                }
                (true, 7) => self.modes.autowrap = on,
                (true, 25) => self.modes.show_cursor = on,
                (true, 47 | 1047) => self.set_alternate(on, false),
                (true, 1048) => {
                    if on {
                        self.save_cursor();
                    } else {
                        self.restore_cursor();
                    }
                }
                (true, 1049) => self.set_alternate(on, true),
                (true, 2004) => self.modes.bracketed_paste = on,
                _ => {}
            }
        }
    }

    fn sgr(&mut self, params: &Params) {
        if params.is_empty() {
            self.cursor.style = Style::default();
            return;
        }
        let style = &mut self.cursor.style;
        let mut i = 0;
        while i < params.len() {
            let code = params.get(i);
            match code {
                0 => *style = Style::default(),
                1 => style.flags |= BOLD,
                2 => style.flags |= DIM,
                3 => style.flags |= ITALIC,
                4 => style.flags |= UNDERLINE,
                7 => style.flags |= INVERSE,
                8 => style.flags |= HIDDEN,
                9 => style.flags |= STRIKE,
                21 => style.flags |= UNDERLINE,
                22 => style.flags &= !(BOLD | DIM),
                23 => style.flags &= !ITALIC,
                24 => style.flags &= !UNDERLINE,
                27 => style.flags &= !INVERSE,
                28 => style.flags &= !HIDDEN,
                29 => style.flags &= !STRIKE,
                30..=37 => style.fg = Color::Indexed((code - 30) as u8),
                39 => style.fg = Color::Default,
                40..=47 => style.bg = Color::Indexed((code - 40) as u8),
                49 => style.bg = Color::Default,
                90..=97 => style.fg = Color::Indexed((code - 90 + 8) as u8),
                100..=107 => style.bg = Color::Indexed((code - 100 + 8) as u8),
                38 | 48 => {
                    let (color, used) = extended_color(params, i);
                    if let Some(color) = color {
                        if code == 38 {
                            style.fg = color;
                        } else {
                            style.bg = color;
                        }
                    }
                    i += used;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn device_status(&mut self, params: &Params, private: Option<u8>) {
        match (private, params.get(0)) {
            (None, 5) => self.responses.extend_from_slice(b"\x1b[0n"),
            (None | Some(b'?'), 6) => {
                let row = if self.modes.origin {
                    self.cursor.row - self.scroll_top
                } else {
                    self.cursor.row
                };
                let reply = format!(
                    "\x1b[{}{};{}R",
                    if private.is_some() { "?" } else { "" },
                    row + 1,
                    self.cursor.col.min(self.cols - 1) + 1
                );
                self.responses.extend_from_slice(reply.as_bytes());
            }
            _ => {}
        }
    }

    fn osc_color_query(&mut self, which: &str, value: &str) {
        let index = match which {
            "10" => 0,
            "11" => 1,
            _ => return,
        };
        if value != "?" {
            return;
        }
        let (r, g, b) = self.default_colors[index];
        let reply = format!("\x1b]{which};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x1b\\");
        self.responses.extend_from_slice(reply.as_bytes());
    }
}

impl Perform for Terminal {
    fn print(&mut self, ch: char) {
        self.print_char(ch);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => self.bell = true,
            0x08 => {
                self.cursor.pending_wrap = false;
                self.cursor.col = self.cursor.col.saturating_sub(1);
            }
            0x09 => {
                let next = (self.cursor.col + 1..self.cols)
                    .find(|&col| self.tabs[col])
                    .unwrap_or(self.cols - 1);
                self.cursor.col = next;
            }
            0x0a..=0x0c => self.line_feed(),
            0x0d => {
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
            }
            0x0e => self.active_charset = 1,
            0x0f => self.active_charset = 0,
            _ => {}
        }
    }

    fn csi(&mut self, params: &Params, private: Option<u8>, intermediates: &[u8], action: char) {
        let count = |index: usize| usize::from(params.get_or(index, 1));
        match (private, intermediates, action) {
            (None, [], '@') => {
                let row = self.cursor.row;
                let col = self.cursor.col;
                let cols = self.cols;
                let blank = self.blank_cell();
                let line = self.screen_line(row);
                for _ in 0..count(0).min(cols - col) {
                    line.cells.insert(col, blank);
                }
                line.cells.truncate(cols);
                if line.cells[cols - 1].width == 2 {
                    line.cells[cols - 1] = blank;
                }
            }
            (None, [], 'A') => {
                let top = if self.cursor.row >= self.scroll_top {
                    self.scroll_top
                } else {
                    0
                };
                self.cursor.row = self.cursor.row.saturating_sub(count(0)).max(top);
                self.cursor.pending_wrap = false;
            }
            (None, [], 'B' | 'e') => {
                let bottom = if self.cursor.row <= self.scroll_bottom {
                    self.scroll_bottom
                } else {
                    self.rows - 1
                };
                self.cursor.row = (self.cursor.row + count(0)).min(bottom);
                self.cursor.pending_wrap = false;
            }
            (None, [], 'C' | 'a') => {
                self.cursor.col = (self.cursor.col + count(0)).min(self.cols - 1);
                self.cursor.pending_wrap = false;
            }
            (None, [], 'D') => {
                self.cursor.col = self.cursor.col.saturating_sub(count(0));
                self.cursor.pending_wrap = false;
            }
            (None, [], 'E') => {
                self.cursor.row =
                    (self.cursor.row + count(0)).min(self.scroll_bottom.max(self.cursor.row));
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
            }
            (None, [], 'F') => {
                self.cursor.row = self.cursor.row.saturating_sub(count(0));
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
            }
            (None, [], 'G' | '`') => {
                self.cursor.col = (count(0) - 1).min(self.cols - 1);
                self.cursor.pending_wrap = false;
            }
            (None, [], 'H' | 'f') => self.move_to(count(0) - 1, count(1) - 1),
            (None, [], 'I') => {
                for _ in 0..count(0) {
                    self.execute(0x09);
                }
            }
            (None | Some(b'?'), [], 'J') => match params.get(0) {
                0 => {
                    let row = self.cursor.row;
                    self.erase_cells(row, self.cursor.col, self.cols);
                    self.erase_rows(row + 1, self.rows);
                }
                1 => {
                    let row = self.cursor.row;
                    self.erase_rows(0, row);
                    self.erase_cells(row, 0, self.cursor.col + 1);
                }
                2 => self.erase_rows(0, self.rows),
                3 if !self.alt_active => {
                    let keep = self.primary.len() - self.rows;
                    self.primary.drain(..keep);
                }
                _ => {}
            },
            (None | Some(b'?'), [], 'K') => {
                let row = self.cursor.row;
                match params.get(0) {
                    0 => self.erase_cells(row, self.cursor.col, self.cols),
                    1 => self.erase_cells(row, 0, self.cursor.col + 1),
                    2 => self.erase_cells(row, 0, self.cols),
                    _ => {}
                }
            }
            (None, [], 'L') => {
                if (self.scroll_top..=self.scroll_bottom).contains(&self.cursor.row) {
                    let top = self.scroll_top;
                    self.scroll_top = self.cursor.row;
                    self.scroll_down(count(0));
                    self.scroll_top = top;
                    self.cursor.col = 0;
                }
            }
            (None, [], 'M') => {
                if (self.scroll_top..=self.scroll_bottom).contains(&self.cursor.row) {
                    let top = self.scroll_top;
                    self.scroll_top = self.cursor.row;
                    // Deleted lines never go to the scrollback.
                    let alt = self.alt_active;
                    let full = self.scroll_top == 0 && self.scroll_bottom == self.rows - 1;
                    if !alt && full {
                        let row = self.cursor.row;
                        let n = count(0).min(self.rows - row);
                        let blank = self.blank_cell();
                        let cols = self.cols;
                        let start = self.primary.len() - self.rows;
                        for _ in 0..n {
                            let mut line = self.primary.remove(start + row).expect("on screen");
                            line.reset(cols, blank);
                            self.primary.push_back(line);
                        }
                    } else {
                        self.scroll_up(count(0));
                    }
                    self.scroll_top = top;
                    self.cursor.col = 0;
                }
            }
            (None, [], 'P') => {
                let row = self.cursor.row;
                let col = self.cursor.col;
                let cols = self.cols;
                let blank = self.blank_cell();
                let n = count(0).min(cols - col);
                let line = self.screen_line(row);
                line.cells.drain(col..col + n);
                line.cells.resize(cols, blank);
                if line.cells[col].width == 0 {
                    line.cells[col] = blank;
                }
            }
            (None, [], 'S') => self.scroll_up(count(0)),
            (None, [], 'T') => self.scroll_down(count(0)),
            (None, [], 'X') => {
                let row = self.cursor.row;
                let col = self.cursor.col;
                self.erase_cells(row, col, col + count(0));
            }
            (None, [], 'Z') => {
                for _ in 0..count(0) {
                    let previous = (0..self.cursor.col).rev().find(|&col| self.tabs[col]);
                    self.cursor.col = previous.unwrap_or(0);
                }
            }
            (None, [], 'b') => {
                if let Some(ch) = self.last_printed {
                    for _ in 0..count(0).min(65_535) {
                        self.print_char(ch);
                    }
                }
            }
            (None, [], 'c') => {
                if params.get(0) == 0 {
                    // A VT220 with advanced video.
                    self.responses.extend_from_slice(b"\x1b[?62;22c");
                }
            }
            (Some(b'>'), [], 'c') => self.responses.extend_from_slice(b"\x1b[>1;10;0c"),
            (None, [], 'd') => {
                let col = self.cursor.col;
                self.move_to(count(0) - 1, col);
            }
            (None, [], 'g') => match params.get(0) {
                0 => {
                    let col = self.cursor.col;
                    self.tabs[col] = false;
                }
                3 => self.tabs.iter_mut().for_each(|tab| *tab = false),
                _ => {}
            },
            (None, [], 'h') => self.set_mode(params, false, true),
            (None, [], 'l') => self.set_mode(params, false, false),
            (Some(b'?'), [], 'h') => self.set_mode(params, true, true),
            (Some(b'?'), [], 'l') => self.set_mode(params, true, false),
            (None, [], 'm') => self.sgr(params),
            (None | Some(b'?'), [], 'n') => self.device_status(params, private),
            (None, [], 'r') => {
                let top = count(0) - 1;
                let bottom = usize::from(params.get_or(1, self.rows as u16)).min(self.rows) - 1;
                if top < bottom {
                    self.scroll_top = top;
                    self.scroll_bottom = bottom;
                    self.move_to(0, 0);
                }
            }
            (None, [], 's') => self.save_cursor(),
            (None, [], 'u') => self.restore_cursor(),
            (None, [b'!'], 'p') => {
                // Soft reset.
                self.modes = Modes::default();
                self.cursor.style = Style::default();
                self.scroll_top = 0;
                self.scroll_bottom = self.rows - 1;
                self.charsets = [Charset::Ascii; 2];
                self.active_charset = 0;
            }
            _ => {}
        }
    }

    fn esc(&mut self, intermediates: &[u8], byte: u8) {
        match (intermediates, byte) {
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([], b'D') => self.line_feed(),
            ([], b'E') => {
                self.cursor.col = 0;
                self.line_feed();
            }
            ([], b'H') => {
                let col = self.cursor.col;
                self.tabs[col] = true;
            }
            ([], b'M') => self.reverse_index(),
            ([], b'c') => self.full_reset(),
            ([b'('], set) => self.charsets[0] = charset(set),
            ([b')'], set) => self.charsets[1] = charset(set),
            ([b'#'], b'8') => {
                // Screen alignment test: fill with E.
                for row in 0..self.rows {
                    self.screen_line(row).cells.fill(Cell {
                        ch: 'E',
                        ..Cell::BLANK
                    });
                }
            }
            _ => {}
        }
    }

    fn osc(&mut self, data: &[u8]) {
        let text = String::from_utf8_lossy(data);
        let (which, value) = text.split_once(';').unwrap_or((&text, ""));
        match which {
            "0" | "2" => self.title = value.chars().take(256).collect(),
            "7" => {
                // file://host/path, the path percent-encoded.
                let path = value
                    .strip_prefix("file://")
                    .and_then(|rest| rest.find('/').map(|slash| &rest[slash..]));
                self.cwd = path.map(|path| PathBuf::from(percent_decode(path)));
            }
            "10" | "11" => {
                let which = which.to_string();
                let value = value.to_string();
                self.osc_color_query(&which, &value);
            }
            _ => {}
        }
    }
}

fn default_tabs(cols: usize) -> Vec<bool> {
    (0..cols).map(|col| col % 8 == 0 && col > 0).collect()
}

fn charset(byte: u8) -> Charset {
    match byte {
        b'0' => Charset::Graphics,
        _ => Charset::Ascii,
    }
}

/// The DEC special graphics character set, used for line drawing.
fn dec_graphics(ch: char) -> char {
    match ch {
        '`' => '◆',
        'a' => '▒',
        'f' => '°',
        'g' => '±',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        other => other,
    }
}

/// An SGR 38/48 color at `params[at]`: `;5;n`, `;2;r;g;b` or their `:`
/// forms. Returns the color and how many parameters it used.
fn extended_color(params: &Params, at: usize) -> (Option<Color>, usize) {
    let byte = |index: usize| params.get(index).min(255) as u8;
    if params.has_sub(at) {
        // 38:5:n or 38:2:[colorspace]:r:g:b, one colon group.
        let mut end = at;
        while params.has_sub(end) {
            end += 1;
        }
        let used = end - at + 1;
        let color = match params.get(at + 1) {
            5 => Some(Color::Indexed(byte(at + 2))),
            2 if used >= 6 => Some(Color::Rgb(byte(at + 3), byte(at + 4), byte(at + 5))),
            2 if used == 5 => Some(Color::Rgb(byte(at + 2), byte(at + 3), byte(at + 4))),
            _ => None,
        };
        return (color, used);
    }
    match params.get(at + 1) {
        5 => (Some(Color::Indexed(byte(at + 2))), 3),
        2 => (
            Some(Color::Rgb(byte(at + 2), byte(at + 3), byte(at + 4))),
            5,
        ),
        _ => (None, 1),
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(value) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(term: &Terminal) -> Vec<String> {
        let start = term.scrollback_len();
        (start..start + term.rows())
            .map(|index| term.line(index).unwrap().text())
            .collect()
    }

    fn fed(cols: usize, rows: usize, bytes: &[u8]) -> Terminal {
        let mut term = Terminal::new(cols, rows);
        term.feed(bytes);
        term
    }

    #[test]
    fn text_wraps_and_scrolls_into_the_scrollback() {
        let term = fed(5, 2, b"helloworld\r\nnext");
        assert_eq!(screen(&term), ["world", "next"]);
        assert_eq!(term.scrollback_len(), 1);
        assert_eq!(term.line(0).unwrap().text(), "hello");
        assert!(term.line(0).unwrap().wrapped);
        assert_eq!(term.cursor(), Some((1, 4)));
    }

    #[test]
    fn the_last_column_waits_before_wrapping() {
        let term = fed(4, 2, b"abcd\r\n");
        assert_eq!(screen(&term), ["abcd", ""]);
        assert!(!term.line(0).unwrap().wrapped);
    }

    #[test]
    fn cursor_moves_and_erases() {
        let term = fed(
            10,
            3,
            b"0123456789\x1b[2;3Hxy\x1b[1;5H\x1b[K\x1b[3;1Hzz\x1b[1D\x1b[1P",
        );
        assert_eq!(screen(&term), ["0123", "  xy", "z"]);
        let term = fed(6, 3, b"aaaaaa\r\nbbbbbb\r\ncccccc\x1b[2;3H\x1b[1J");
        assert_eq!(screen(&term), ["", "   bbb", "cccccc"]);
        let term = fed(6, 3, b"aaaaaa\r\nbbbbbb\x1b[1;4H\x1b[0J");
        assert_eq!(screen(&term), ["aaa", "", ""]);
    }

    #[test]
    fn insert_and_delete_lines_inside_a_scroll_region() {
        let term = fed(3, 4, b"a\r\nb\r\nc\r\nd\x1b[2;3r\x1b[2;1H\x1b[L");
        assert_eq!(screen(&term), ["a", "", "b", "d"]);
        let term = fed(3, 4, b"a\r\nb\r\nc\r\nd\x1b[2;3r\x1b[2;1H\x1b[M");
        assert_eq!(screen(&term), ["a", "c", "", "d"]);
        assert_eq!(
            term.scrollback_len(),
            0,
            "regions never feed the scrollback"
        );
    }

    #[test]
    fn colors_and_attributes() {
        let term = fed(
            10,
            1,
            b"\x1b[1;31mA\x1b[38;5;200;48;2;1;2;3mB\x1b[38:2::4:5:6mC\x1b[0mD",
        );
        let cells = &term.line(0).unwrap().cells;
        assert_eq!(cells[0].style.fg, Color::Indexed(1));
        assert_eq!(cells[0].style.flags, BOLD);
        assert_eq!(cells[1].style.fg, Color::Indexed(200));
        assert_eq!(cells[1].style.bg, Color::Rgb(1, 2, 3));
        assert_eq!(cells[2].style.fg, Color::Rgb(4, 5, 6));
        assert_eq!(cells[3].style, Style::default());
    }

    #[test]
    fn the_alternate_screen_keeps_the_normal_one() {
        let mut term = fed(8, 2, b"shell$ ");
        term.feed(b"\x1b[?1049h\x1b[Hfull\x1b[?1049l");
        assert!(!term.alternate_screen());
        assert_eq!(screen(&term), ["shell$", ""]);
        assert_eq!(
            term.cursor(),
            Some((0, 7)),
            "cursor restored past the prompt's space"
        );
    }

    #[test]
    fn replies_to_cursor_position_and_attribute_questions() {
        let mut term = fed(10, 5, b"\x1b[3;4H\x1b[6n\x1b[c\x1b]11;?\x07");
        let replies = String::from_utf8(term.take_responses()).unwrap();
        assert_eq!(
            replies,
            "\x1b[3;4R\x1b[?62;22c\x1b]11;rgb:1414/1919/2121\x1b\\"
        );
    }

    #[test]
    fn wide_characters_take_two_cells_and_line_drawing_maps() {
        let term = fed(4, 2, "日本x".as_bytes());
        assert_eq!(screen(&term), ["日本", "x"]);
        let term = fed(4, 1, b"\x1b(0lqk\x1b(B");
        assert_eq!(screen(&term), ["┌─┐"]);
    }

    #[test]
    fn titles_and_working_directory() {
        let term = fed(
            4,
            1,
            b"\x1b]0;cargo build\x07\x1b]7;file://mac/Users/jay/my%20dir\x1b\\",
        );
        assert_eq!(term.title, "cargo build");
        assert_eq!(term.cwd, Some(PathBuf::from("/Users/jay/my dir")));
    }

    #[test]
    fn resizing_keeps_the_cursor_line() {
        let mut term = fed(6, 4, b"one\r\ntwo\r\n$ ");
        term.resize(6, 2);
        assert_eq!(screen(&term), ["two", "$"]);
        assert_eq!(term.cursor(), Some((1, 2)));
        term.resize(8, 4);
        assert_eq!(screen(&term), ["one", "two", "$", ""]);
        assert_eq!(term.cursor(), Some((2, 2)));
    }

    #[test]
    fn scrollback_is_bounded() {
        let mut term = Terminal::new(4, 2);
        for _ in 0..SCROLLBACK_LINES + 50 {
            term.feed(b"x\r\n");
        }
        assert_eq!(term.scrollback_len(), SCROLLBACK_LINES);
    }
}
