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
