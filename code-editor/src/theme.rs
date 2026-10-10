//! Colors and sizes shared by the editor's panels.

use ui_core::Color;

pub const BACKGROUND: Color = Color::rgba(0x12, 0x16, 0x1d, 0xff);
pub const PANEL: Color = Color::rgba(0x17, 0x1c, 0x25, 0xff);
pub const EDITOR_BACKGROUND: Color = Color::rgba(0x14, 0x19, 0x21, 0xff);
pub const BORDER: Color = Color::rgba(0x2c, 0x34, 0x42, 0xff);
pub const TEXT: Color = Color::rgba(0xdc, 0xe2, 0xec, 0xff);
pub const TEXT_DIM: Color = Color::rgba(0x8b, 0x96, 0xa8, 0xff);
pub const ACCENT: Color = Color::rgba(0x5b, 0x9b, 0xf0, 0xff);
pub const WARNING: Color = Color::rgba(0xe8, 0xb3, 0x4f, 0xff);
pub const ERROR: Color = Color::rgba(0xf0, 0x6b, 0x6b, 0xff);
pub const HOVER: Color = Color::rgba(0x22, 0x2a, 0x37, 0xff);
pub const SELECTED: Color = Color::rgba(0x24, 0x3a, 0x5c, 0xff);
pub const SELECTION_FILL: Color = Color::rgba(0x2f, 0x4f, 0x80, 0xd0);
pub const BUTTON: Color = Color::rgba(0x23, 0x2b, 0x38, 0xff);
pub const BUTTON_HOVER: Color = Color::rgba(0x2e, 0x39, 0x4a, 0xff);

pub const UI_FONT: u16 = 14;
pub const CODE_FONT: u16 = 15;
pub const TOOLBAR_HEIGHT: f32 = 38.0;
pub const TAB_HEIGHT: f32 = 32.0;
pub const STATUS_HEIGHT: f32 = 26.0;
pub const BAR_HEIGHT: f32 = 38.0;
pub const TREE_ROW_HEIGHT: f32 = 24.0;
pub const TREE_INDENT: f32 = 16.0;
pub const TREE_WIDTH: f32 = 280.0;

// Syntax colors.
pub const SYNTAX_KEYWORD: Color = Color::rgba(0xc7, 0x92, 0xea, 0xff);
pub const SYNTAX_TYPE: Color = Color::rgba(0x4f, 0xd6, 0xbe, 0xff);
pub const SYNTAX_FUNCTION: Color = Color::rgba(0x82, 0xaa, 0xff, 0xff);
pub const SYNTAX_MACRO: Color = Color::rgba(0xff, 0xcb, 0x6b, 0xff);
pub const SYNTAX_LIFETIME: Color = Color::rgba(0xff, 0x9e, 0x64, 0xff);
pub const SYNTAX_STRING: Color = Color::rgba(0xc3, 0xe8, 0x8d, 0xff);
pub const SYNTAX_NUMBER: Color = Color::rgba(0xf7, 0x8c, 0x6c, 0xff);
pub const SYNTAX_COMMENT: Color = Color::rgba(0x6a, 0x75, 0x90, 0xff);
pub const SYNTAX_DOC: Color = Color::rgba(0x8f, 0x9d, 0xb8, 0xff);
pub const SYNTAX_ATTRIBUTE: Color = Color::rgba(0x89, 0xdd, 0xff, 0xff);
pub const SYNTAX_PUNCTUATION: Color = Color::rgba(0xa6, 0xae, 0xbd, 0xff);
