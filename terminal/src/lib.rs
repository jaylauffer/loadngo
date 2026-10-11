//! A terminal for loadngo apps.
//!
//! - [`Parser`] splits a program's output into text, controls and escape
//!   sequences.
//! - [`Terminal`] keeps the screen they draw: cells, scrollback, cursor,
//!   modes. It knows nothing about pixels; a view paints its cells.
//! - [`encode_key`], [`encode_paste`] and [`encode_mouse`] turn keys,
//!   pasted text and pointer events into what the program reads.
//! - [`Pty`] runs a program (usually the user's shell) on a
//!   pseudo-terminal: macOS and Linux today; elsewhere `spawn` reports that
//!   terminals are not available yet.

mod keys;
mod parser;
mod pty;
mod screen;
mod width;

pub use keys::{
    encode_key, encode_mouse, encode_paste, Key, Modifiers, MouseAction, MouseButton, MouseEvent,
};
pub use parser::{Params, Parser, Perform};
pub use pty::{Pty, PtyConfig, MAX_PENDING};
pub use screen::{
    Cell, Color, Line, Modes, MouseEncoding, MouseTracking, Style, Terminal, BOLD, DIM, HIDDEN,
    INVERSE, ITALIC, SCROLLBACK_LINES, STRIKE, UNDERLINE,
};
pub use width::char_width;
