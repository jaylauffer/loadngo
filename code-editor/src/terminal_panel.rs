//! The Terminal panel: shells in tabs at the bottom of the window.
//!
//! The panel keeps each terminal's screen ([`loadngo_terminal::Terminal`])
//! and draws it; the app runs the programs ([`loadngo_terminal::Pty`]) and
//! trades bytes with the panel through [`TerminalRequest`]s and
//! [`TerminalPanel::output`]. Every cell sits on a grid of equal widths, as
//! programs that draw tables and full screens need, while each glyph is the
//! editor's own font, centered in its cell.

use std::path::PathBuf;

use loadngo_terminal::{
    encode_key, encode_paste, Cell, Color as TermColor, Key as TermKey, Modifiers as TermModifiers,
    Terminal, BOLD, DIM, HIDDEN, INVERSE, STRIKE, UNDERLINE,
};
use ui_core::{
    Color, HorizontalAlign, PaintOp, Point, Rect, TextLayoutMode, TextOverflow, TextStyle,
    VerticalAlign,
};

use crate::draw;
use crate::theme;

/// What the app should do with the programs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalRequest {
    /// Start the user's shell in `cwd` on a terminal of this size.
    Start {
        id: u64,
        cwd: PathBuf,
        cols: u16,
        rows: u16,
    },
    Write {
        id: u64,
        bytes: Vec<u8>,
    },
    Resize {
        id: u64,
        cols: u16,
        rows: u16,
    },
    /// Hang up on the program and forget it.
    Close {
        id: u64,
    },
}

pub const FONT: u16 = 14;
const HEADER_HEIGHT: f32 = 28.0;
const PADDING: f32 = 6.0;

/// A cell on the terminal: a line counting from the oldest scrollback
/// line, and a column.
type CellPos = (usize, usize);

struct Tab {
    id: u64,
    term: Terminal,
    /// Where the program starts; taken when it is asked to start, which
    /// waits for the panel's first layout so it starts at its real size.
    start_in: Option<PathBuf>,
    /// The program's name, for the tab when it sets no title.
    name: String,
    exited: Option<String>,
    /// Lines scrolled back from the bottom.
    scroll: usize,
    selection: Option<(CellPos, CellPos)>,
    /// Wheel movement not yet a whole line.
    wheel_rest: f32,
}

impl Tab {
    fn label(&self) -> String {
        let title = if self.term.title.is_empty() {
            &self.name
        } else {
            &self.term.title
        };
        match &self.exited {
            Some(_) => format!("{title} (exited)"),
            None => title.clone(),
        }
    }

    /// The first line shown.
    fn first_line(&self) -> usize {
        self.term.scrollback_len().saturating_sub(self.scroll)
    }

    fn ordered_selection(&self) -> Option<(CellPos, CellPos)> {
        let (a, b) = self.selection?;
        (a != b).then(|| if a <= b { (a, b) } else { (b, a) })
    }

    fn selected_text(&self) -> Option<String> {
        let (start, end) = self.ordered_selection()?;
        let mut text = String::new();
        for index in start.0..=end.0 {
            let Some(line) = self.term.line(index) else {
                break;
            };
            let from = if index == start.0 { start.1 } else { 0 };
            let to = if index == end.0 {
                end.1
            } else {
                line.cells.len()
            };
            let piece: String = line
                .cells
                .iter()
                .take(to)
                .skip(from)
                .filter(|cell| cell.width != 0)
                .map(|cell| cell.ch)
                .collect();
            if line.wrapped && index != end.0 {
                text.push_str(&piece);
            } else {
                text.push_str(piece.trim_end());
                if index != end.0 {
                    text.push('\n');
                }
            }
        }
        Some(text)
    }
}

#[derive(Default)]
struct PanelLayout {
    header: Rect,
    tabs: Vec<(Rect, Rect)>,
    new_button: Rect,
    body: Rect,
}

#[derive(Default)]
pub struct TerminalPanel {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    outbox: Vec<TerminalRequest>,
    layout: PanelLayout,
    /// A cell's width and height.
    cell: (f32, f32),
    /// The screen size in cells, from the last layout.
    size: (usize, usize),
    /// The pointer is pressed in the body, selecting.
    selecting: bool,
}

impl TerminalPanel {
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn take_requests(&mut self) -> Vec<TerminalRequest> {
        std::mem::take(&mut self.outbox)
    }

    /// Starts a new shell in `cwd`, in a tab of its own.
    pub fn open(&mut self, cwd: PathBuf, name: String) {
        let id = self.next_id;
        self.next_id += 1;
        let (cols, rows) = if self.size.0 > 0 { self.size } else { (80, 24) };
        self.tabs.push(Tab {
            id,
            term: Terminal::new(cols, rows),
            start_in: Some(cwd),
            name,
            exited: None,
            scroll: 0,
            selection: None,
            wheel_rest: 0.0,
        });
        self.active = self.tabs.len() - 1;
        if self.size.0 > 0 {
            self.start_pending();
        }
    }

    /// Asks for the programs of tabs not started yet, at the panel's size.
    fn start_pending(&mut self) {
        let (cols, rows) = self.size;
        for tab in &mut self.tabs {
            if let Some(cwd) = tab.start_in.take() {
                tab.term.resize(cols, rows);
                self.outbox.push(TerminalRequest::Start {
                    id: tab.id,
                    cwd,
                    cols: cols as u16,
                    rows: rows as u16,
                });
            }
        }
    }

    fn close(&mut self, index: usize) {
        let tab = self.tabs.remove(index);
        self.outbox.push(TerminalRequest::Close { id: tab.id });
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
    }

    /// Closes every terminal (the app is quitting or the folder changed).
    pub fn close_all(&mut self) {
        while !self.tabs.is_empty() {
            self.close(0);
        }
    }

    fn tab_mut(&mut self, id: u64) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    /// Output from terminal `id`'s program.
    pub fn output(&mut self, id: u64, bytes: &[u8]) {
        let Some(tab) = self.tab_mut(id) else {
            return;
        };
        tab.term.feed(bytes);
        let replies = tab.term.take_responses();
        let scrolled = tab.term.take_scrolled();
        if tab.scroll > 0 {
            // Keep a view scrolled back on the same lines.
            tab.scroll = (tab.scroll + scrolled).min(tab.term.scrollback_len());
        }
        let _ = tab.term.take_bell();
        if !replies.is_empty() {
            self.outbox
                .push(TerminalRequest::Write { id, bytes: replies });
        }
    }

    /// Terminal `id`'s program ended.
    pub fn ended(&mut self, id: u64, reason: String) {
        if let Some(tab) = self.tab_mut(id) {
            tab.exited = Some(reason);
        }
    }

    /// The working directory the active shell last reported.
    pub fn active_cwd(&self) -> Option<PathBuf> {
        self.tabs.get(self.active)?.term.cwd.clone()
    }

    pub fn layout(&mut self, panel: Rect, measure: &dyn Fn(&str, u16) -> f32) {
        if self.cell.0 <= 0.0 {
            let digits = measure("0123456789", FONT) / 10.0;
            let letters = measure("abcdefghijklmnopqrstuvwxyz", FONT) / 26.0;
            self.cell = (
                (digits.max(letters) * 2.0).ceil() / 2.0,
                (f32::from(FONT) * 1.3).round(),
            );
        }
        let header = Rect {
            height: HEADER_HEIGHT,
            ..panel
        };
        let mut x = header.x + 8.0;
        let mut tabs = Vec::new();
        for tab in &self.tabs {
            let width = (measure(&tab.label(), theme::UI_FONT) + 44.0).clamp(90.0, 260.0);
            let rect = Rect {
                x,
                y: header.y + 2.0,
                width,
                height: HEADER_HEIGHT - 4.0,
            };
            let close = Rect {
                x: rect.right() - 22.0,
                y: rect.y + (rect.height - 16.0) / 2.0,
                width: 16.0,
                height: 16.0,
            };
            tabs.push((rect, close));
            x += width + 4.0;
        }
        let new_button = Rect {
            x,
            y: header.y + 3.0,
            width: 26.0,
            height: HEADER_HEIGHT - 6.0,
        };
        let body = Rect {
            x: panel.x,
            y: panel.y + HEADER_HEIGHT,
            width: panel.width,
            height: (panel.height - HEADER_HEIGHT).max(0.0),
        };
        self.layout = PanelLayout {
            header,
            tabs,
            new_button,
            body,
        };
        let cols = (((body.width - 2.0 * PADDING) / self.cell.0).floor() as usize).max(2);
        let rows = (((body.height - 2.0 * PADDING) / self.cell.1).floor() as usize).max(1);
        if (cols, rows) != self.size {
            self.size = (cols, rows);
            for tab in &mut self.tabs {
                tab.term.resize(cols, rows);
                tab.scroll = tab.scroll.min(tab.term.scrollback_len());
                if tab.exited.is_none() && tab.start_in.is_none() {
                    self.outbox.push(TerminalRequest::Resize {
                        id: tab.id,
                        cols: cols as u16,
                        rows: rows as u16,
                    });
                }
            }
        }
        self.start_pending();
    }

    // ----- input -----

    fn send(&mut self, bytes: Vec<u8>) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if tab.exited.is_some() || bytes.is_empty() {
            return;
        }
        // Typing brings the view back to the bottom.
        tab.scroll = 0;
        tab.selection = None;
        self.outbox
            .push(TerminalRequest::Write { id: tab.id, bytes });
    }

    pub fn text(&mut self, text: &str) {
        self.send(text.as_bytes().to_vec());
    }

    pub fn paste(&mut self, text: &str) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let mut bytes = Vec::new();
        encode_paste(text, tab.term.modes.bracketed_paste, &mut bytes);
        self.send(bytes);
    }

    /// A key that is not text; false when the terminal does not take it.
    pub fn key(&mut self, key: loadngo_host_core::HostKey, modifiers: ui_core::Modifiers) -> bool {
        use loadngo_host_core::HostKey;
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return false;
        };
        if modifiers.meta {
            // The macOS line moves, as the editor's own text areas do them.
            let byte = match key {
                HostKey::Left => 0x01,
                HostKey::Right => 0x05,
                HostKey::Backspace => 0x15,
                _ => return false,
            };
            self.send(vec![byte]);
            return true;
        }
        if modifiers.shift && matches!(key, HostKey::PageUp | HostKey::PageDown) {
            let page = tab.term.rows().saturating_sub(1).max(1);
            tab.scroll = if key == HostKey::PageUp {
                (tab.scroll + page).min(tab.term.scrollback_len())
            } else {
                tab.scroll.saturating_sub(page)
            };
            return true;
        }
        let key = match key {
            HostKey::Up => TermKey::Up,
            HostKey::Down => TermKey::Down,
            HostKey::Left => TermKey::Left,
            HostKey::Right => TermKey::Right,
            HostKey::Home => TermKey::Home,
            HostKey::End => TermKey::End,
            HostKey::PageUp => TermKey::PageUp,
            HostKey::PageDown => TermKey::PageDown,
            HostKey::Delete => TermKey::Delete,
            HostKey::Backspace => TermKey::Backspace,
            HostKey::Enter => TermKey::Enter,
            HostKey::Tab => TermKey::Tab,
            HostKey::Escape => TermKey::Escape,
            // Letters and Space arrive as text or control characters.
            _ => return false,
        };
        let mut bytes = Vec::new();
        encode_key(
            key,
            TermModifiers {
                shift: modifiers.shift,
                alt: modifiers.alt,
                ctrl: modifiers.ctrl,
            },
            tab.term.modes.app_cursor,
            &mut bytes,
        );
        self.send(bytes);
        true
    }

    pub fn copy(&self) -> Option<String> {
        self.tabs.get(self.active)?.selected_text()
    }

    pub fn select_all(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            let last = tab.term.scrollback_len() + tab.term.rows() - 1;
            tab.selection = Some(((0, 0), (last, tab.term.cols())));
        }
    }

    fn cell_at(&self, point: Point) -> Option<CellPos> {
        let tab = self.tabs.get(self.active)?;
        let body = self.layout.body;
        let col = ((point.x - body.x - PADDING) / self.cell.0)
            .floor()
            .max(0.0) as usize;
        let row = ((point.y - body.y - PADDING) / self.cell.1)
            .floor()
            .max(0.0) as usize;
        Some((
            tab.first_line() + row.min(tab.term.rows() - 1),
            col.min(tab.term.cols()),
        ))
    }

    /// A press in the panel; true when it took the press. Clicking the body
    /// focuses the terminal and starts a selection.
    pub fn pressed(&mut self, point: Point, cwd: &dyn Fn() -> (PathBuf, String)) -> bool {
        if !self.layout.header.contains(point) && !self.layout.body.contains(point) {
            return false;
        }
        if self.layout.new_button.contains(point) {
            let (cwd, name) = cwd();
            self.open(cwd, name);
            return true;
        }
        if let Some(index) = self
            .layout
            .tabs
            .iter()
            .position(|(tab, _)| tab.contains(point))
        {
            if self.layout.tabs[index].1.contains(point) {
                self.close(index);
            } else {
                self.active = index;
            }
            return true;
        }
        if self.layout.body.contains(point) {
            if let Some(cell) = self.cell_at(point) {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.selection = Some((cell, cell));
                    self.selecting = true;
                }
            }
        }
        true
    }

    pub fn dragged(&mut self, point: Point) {
        if !self.selecting {
            return;
        }
        let Some(cell) = self.cell_at(point) else {
            return;
        };
        if let Some(tab) = self.tabs.get_mut(self.active) {
            if let Some((start, _)) = tab.selection {
                tab.selection = Some((start, cell));
            }
        }
    }

    pub fn released(&mut self) {
        self.selecting = false;
    }

    /// The wheel over the panel: scrolls back through the output, or on a
    /// full-screen program's alternate screen sends it arrow keys.
    pub fn wheel(&mut self, delta_y: f32, precise: bool) {
        let row_height = self.cell.1.max(1.0);
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        tab.wheel_rest += if precise {
            delta_y / row_height
        } else {
            delta_y * 3.0
        };
        let lines = tab.wheel_rest.trunc();
        tab.wheel_rest -= lines;
        if lines == 0.0 {
            return;
        }
        if tab.term.alternate_screen() {
            let key = if lines > 0.0 {
                TermKey::Up
            } else {
                TermKey::Down
            };
            let mut bytes = Vec::new();
            for _ in 0..(lines.abs() as usize).min(50) {
                encode_key(
                    key,
                    TermModifiers::default(),
                    tab.term.modes.app_cursor,
                    &mut bytes,
                );
            }
            let id = tab.id;
            if tab.exited.is_none() {
                self.outbox.push(TerminalRequest::Write { id, bytes });
            }
            return;
        }
        let scroll = tab.scroll as isize + lines as isize;
        tab.scroll = scroll.clamp(0, tab.term.scrollback_len() as isize) as usize;
    }

    // ----- painting -----

    pub fn paint(&self, scene: &mut Vec<PaintOp>, focused: bool) {
        let layout = &self.layout;
        draw::fill(scene, layout.header, theme::PANEL);
        draw::hline(
            scene,
            layout.header.x,
            layout.header.y + 0.5,
            layout.header.width,
            theme::BORDER,
        );
        for (index, (tab, (rect, close))) in self.tabs.iter().zip(&layout.tabs).enumerate() {
            if index == self.active {
                draw::fill(scene, *rect, theme::SELECTED);
            }
            draw::label(
                scene,
                &tab.label(),
                Rect {
                    x: rect.x + 10.0,
                    width: (close.x - rect.x - 14.0).max(0.0),
                    ..*rect
                },
                if index == self.active {
                    theme::TEXT
                } else {
                    theme::TEXT_DIM
                },
                theme::UI_FONT,
                HorizontalAlign::Left,
            );
            draw::label(
                scene,
                "×",
                *close,
                theme::TEXT_DIM,
                theme::UI_FONT,
                HorizontalAlign::Center,
            );
        }
        draw::button(scene, layout.new_button, "+", false, true);
        let body = layout.body;
        draw::fill(scene, body, theme::EDITOR_BACKGROUND);
        let Some(tab) = self.tabs.get(self.active) else {
            draw::label(
                scene,
                "No terminal. Click + to start one.",
                Rect {
                    x: body.x + 12.0,
                    width: body.width - 24.0,
                    ..body
                },
                theme::TEXT_DIM,
                theme::UI_FONT,
                HorizontalAlign::Left,
            );
            return;
        };
        let (cell_w, cell_h) = self.cell;
        let origin = Point {
            x: body.x + PADDING,
            y: body.y + PADDING,
        };
        let first = tab.first_line();
        let selection = tab.ordered_selection();
        let cursor = (tab.scroll == 0)
            .then(|| tab.term.cursor())
            .flatten()
            .filter(|_| tab.exited.is_none());
        for row in 0..tab.term.rows() {
            let index = first + row;
            let Some(line) = tab.term.line(index) else {
                break;
            };
            let y = origin.y + row as f32 * cell_h;
            // Backgrounds, in runs of one color.
            let mut col = 0;
            while col < line.cells.len() {
                let bg = cell_colors(&line.cells[col]).1;
                let start = col;
                while col < line.cells.len() && cell_colors(&line.cells[col]).1 == bg {
                    col += 1;
                }
                if let Some(bg) = bg {
                    draw::fill(
                        scene,
                        Rect {
                            x: origin.x + start as f32 * cell_w,
                            y,
                            width: (col - start) as f32 * cell_w,
                            height: cell_h,
                        },
                        bg,
                    );
                }
            }
            if let Some(((start_line, start_col), (end_line, end_col))) = selection {
                if (start_line..=end_line).contains(&index) {
                    let from = if index == start_line { start_col } else { 0 };
                    let to = if index == end_line {
                        end_col
                    } else {
                        tab.term.cols()
                    };
                    if to > from {
                        draw::fill(
                            scene,
                            Rect {
                                x: origin.x + from as f32 * cell_w,
                                y,
                                width: (to - from) as f32 * cell_w,
                                height: cell_h,
                            },
                            theme::SELECTION_FILL,
                        );
                    }
                }
            }
            for (col, cell) in line.cells.iter().enumerate() {
                if cell.width == 0 {
                    continue;
                }
                let x = origin.x + col as f32 * cell_w;
                let width = cell_w * f32::from(cell.width);
                let at_cursor = cursor == Some((row, col));
                let (mut fg, _) = cell_colors(cell);
                if at_cursor {
                    let block = Rect {
                        x,
                        y,
                        width,
                        height: cell_h,
                    };
                    if focused {
                        draw::fill(scene, block, theme::TEXT);
                        fg = theme::EDITOR_BACKGROUND;
                    } else {
                        draw::outline(scene, block, theme::TEXT_DIM);
                    }
                }
                if cell.style.flags & UNDERLINE != 0 {
                    draw::hline(scene, x, y + cell_h - 2.0, width, fg);
                }
                if cell.style.flags & STRIKE != 0 {
                    draw::hline(scene, x, y + cell_h / 2.0, width, fg);
                }
                if cell.ch == ' ' || cell.style.flags & HIDDEN != 0 {
                    continue;
                }
                // The glyph centered on its cell, free to overhang it a
                // little: proportional glyphs differ in width.
                scene.push(PaintOp::Text {
                    rect: Rect {
                        x: x - cell_w,
                        y,
                        width: width + 2.0 * cell_w,
                        height: cell_h,
                    },
                    clip_rect: Some(body),
                    text: cell.ch.to_string(),
                    style: TextStyle {
                        color: fg,
                        font_size: FONT,
                        horizontal_align: HorizontalAlign::Center,
                        vertical_align: VerticalAlign::Middle,
                        layout_mode: TextLayoutMode::SingleLine,
                        overflow: TextOverflow::Clip,
                        ..TextStyle::default()
                    },
                });
            }
        }
        if tab.scroll > 0 {
            draw::label(
                scene,
                &format!("{} lines up — type to return", tab.scroll),
                Rect {
                    x: body.right() - 260.0,
                    y: body.y + 4.0,
                    width: 250.0,
                    height: 20.0,
                },
                theme::TEXT_DIM,
                theme::UI_FONT,
                HorizontalAlign::Right,
            );
        }
        if let Some(reason) = &tab.exited {
            draw::label(
                scene,
                &format!("{reason}. Close the tab, or click + for a new terminal."),
                Rect {
                    x: body.x + 12.0,
                    y: body.bottom() - 24.0,
                    width: body.width - 24.0,
                    height: 20.0,
                },
                theme::WARNING,
                theme::UI_FONT,
                HorizontalAlign::Left,
            );
        }
    }
}

const PALETTE: [(u8, u8, u8); 16] = [
    (0x3b, 0x42, 0x52),
    (0xe0, 0x6c, 0x75),
    (0x98, 0xc3, 0x79),
    (0xe5, 0xc0, 0x7b),
    (0x61, 0xaf, 0xef),
    (0xc6, 0x78, 0xdd),
    (0x56, 0xb6, 0xc2),
    (0xab, 0xb2, 0xbf),
    (0x5c, 0x63, 0x70),
    (0xff, 0x7a, 0x85),
    (0xb5, 0xe8, 0x90),
    (0xff, 0xd6, 0x8a),
    (0x82, 0xc4, 0xff),
    (0xdd, 0x9c, 0xf0),
    (0x7f, 0xd1, 0xdb),
    (0xe6, 0xe9, 0xef),
];

fn indexed(index: u8) -> Color {
    let (r, g, b) = match index {
        0..=15 => PALETTE[usize::from(index)],
        16..=231 => {
            let level = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            let index = index - 16;
            (level(index / 36), level(index / 6 % 6), level(index % 6))
        }
        _ => {
            let gray = 8 + (index - 232) * 10;
            (gray, gray, gray)
        }
    };
    Color::rgba(r, g, b, 0xff)
}

fn resolve(color: TermColor, bold: bool) -> Option<Color> {
    match color {
        TermColor::Default => None,
        // Bold text in one of the first eight colors shows bright.
        TermColor::Indexed(index) if bold && index < 8 => Some(indexed(index + 8)),
        TermColor::Indexed(index) => Some(indexed(index)),
        TermColor::Rgb(r, g, b) => Some(Color::rgba(r, g, b, 0xff)),
    }
}

/// A cell's text color and background (`None`: the panel's own).
fn cell_colors(cell: &Cell) -> (Color, Option<Color>) {
    let flags = cell.style.flags;
    let fg = resolve(cell.style.fg, flags & BOLD != 0);
    let bg = resolve(cell.style.bg, false);
    let (fg, bg) = if flags & INVERSE != 0 {
        (
            bg.unwrap_or(theme::EDITOR_BACKGROUND),
            Some(fg.unwrap_or(theme::TEXT)),
        )
    } else {
        (fg.unwrap_or(theme::TEXT), bg)
    };
    let fg = if flags & DIM != 0 {
        Color::rgba(fg.r / 2 + 0x0a, fg.g / 2 + 0x0c, fg.b / 2 + 0x10, fg.a)
    } else {
        fg
    };
    (fg, bg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measure(text: &str, size: u16) -> f32 {
        text.chars().count() as f32 * f32::from(size) * 0.5
    }

    fn panel() -> TerminalPanel {
        let mut panel = TerminalPanel::default();
        panel.layout(
            Rect {
                x: 0.0,
                y: 0.0,
                width: 600.0,
                height: 300.0,
            },
            &measure,
        );
        panel.open(PathBuf::from("/w"), "zsh".to_string());
        panel
    }

    #[test]
    fn a_new_terminal_starts_at_the_panel_size_and_follows_resizes() {
        let mut panel = panel();
        let requests = panel.take_requests();
        let TerminalRequest::Start { cols, rows, .. } = requests[0] else {
            panic!("{requests:?}");
        };
        assert_eq!((usize::from(cols), usize::from(rows)), panel.size);
        panel.layout(
            Rect {
                x: 0.0,
                y: 0.0,
                width: 300.0,
                height: 300.0,
            },
            &measure,
        );
        assert!(matches!(
            panel.take_requests()[..],
            [TerminalRequest::Resize { cols, .. }] if usize::from(cols) == panel.size.0
        ));
    }

    #[test]
    fn keys_text_and_replies_go_to_the_program() {
        let mut panel = panel();
        panel.take_requests();
        panel.text("ls");
        assert!(panel.key(loadngo_host_core::HostKey::Enter, Default::default()));
        assert!(!panel.key(loadngo_host_core::HostKey::C, Default::default()));
        panel.output(0, b"\x1b[6n");
        let written: Vec<Vec<u8>> = panel
            .take_requests()
            .into_iter()
            .filter_map(|request| match request {
                TerminalRequest::Write { bytes, .. } => Some(bytes),
                _ => None,
            })
            .collect();
        assert_eq!(
            written,
            [b"ls".to_vec(), b"\r".to_vec(), b"\x1b[1;1R".to_vec()]
        );
    }

    #[test]
    fn selections_copy_lines_and_the_wheel_scrolls_back() {
        let mut panel = panel();
        for line in 0..100 {
            panel.output(0, format!("line {line}\r\n").as_bytes());
        }
        panel.wheel(2.0, false);
        let tab = &panel.tabs[0];
        assert_eq!(tab.scroll, 6);
        let first = tab.first_line();
        let start = first;
        panel.tabs[0].selection = Some(((start, 0), (start + 1, 3)));
        let text = panel.copy().unwrap();
        let expected_first = panel.tabs[0].term.line(start).unwrap().text();
        assert_eq!(text, format!("{expected_first}\nlin"));
        // Typing returns to the bottom.
        panel.text("x");
        assert_eq!(panel.tabs[0].scroll, 0);
    }
}
