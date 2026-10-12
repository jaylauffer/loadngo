//! The screens every loadngo game shows around play: a title menu (Start,
//! Sound, Achievements, Quit), a sound settings panel and an achievements
//! list.
//!
//! Each screen is a model the game keeps across frames. Per frame the game
//! calls `update` with the host's input, acts on what it returns, then calls
//! `paint` to append `RenderOp`s, which it renders with
//! `loadngo_host_desktop::render_ops`. The game supplies every string
//! (already localized), its colours ([`MenuTheme`]), and owns persistence
//! and the audio mixer; this crate owns layout and input.
//!
//! Mouse, touch, keyboard and gamepad all work on every screen: pointer
//! press-and-release on a button, arrow keys or d-pad/stick to move focus,
//! Enter/Space/South to confirm, Escape/East to go back. A screen starts
//! disarmed and ignores keys and pad buttons until they have been seen
//! released, so a press that opened it cannot also act on it.
//!
//! Screens are static unless something moves: see [`menu_frame_demand`].
//! See `docs/GAME_MENU.md`.

mod achievements;
mod input;
mod paint;
mod sound;
mod title;

use std::time::Duration;

use loadngo_host_core::{FrameDemand, SafeAreaInsets, SurfaceInfo};
use ui_core::{Color, Rect};

pub use achievements::{AchievementRow, AchievementsPanel, AchievementsText};
pub use sound::{SoundChannel, SoundLevel, SoundPanel, SoundPanelResponse};
pub use title::{TitleAction, TitleMenu};

/// The colours a game's menu screens are drawn in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MenuTheme {
    /// Fills the screen behind the title menu.
    pub backdrop: Color,
    /// Dims the screen behind a panel.
    pub scrim: Color,
    pub panel: Color,
    pub panel_border: Color,
    pub title: Color,
    pub text: Color,
    pub muted_text: Color,
    pub button: Color,
    pub button_highlight: Color,
    pub button_border: Color,
    /// Border of the highlighted (focused, hovered or pressed) control.
    pub highlight_border: Color,
    /// Slider fill and the scroll thumb.
    pub accent: Color,
    /// Earned achievements.
    pub earned: Color,
    /// Scroll track and empty slider track.
    pub track: Color,
}

impl Default for MenuTheme {
    /// `sng-roguelite`'s palette: near-black blue with a cyan accent and
    /// gold for earned achievements.
    fn default() -> Self {
        Self {
            backdrop: Color::rgba(0x0c, 0x10, 0x18, 0xff),
            scrim: Color::rgba(0x04, 0x07, 0x0d, 0xb8),
            panel: Color::rgba(0x17, 0x20, 0x2c, 0xf5),
            panel_border: Color::rgba(0x8e, 0xdd, 0xff, 0xff),
            title: Color::rgba(0xe8, 0xee, 0xf5, 0xff),
            text: Color::rgba(0xe8, 0xee, 0xf5, 0xff),
            muted_text: Color::rgba(0x9a, 0xa8, 0xb8, 0xff),
            button: Color::rgba(0x1f, 0x2b, 0x3a, 0xff),
            button_highlight: Color::rgba(0x2b, 0x4a, 0x4f, 0xff),
            button_border: Color::rgba(0x8b, 0xe0, 0xd0, 0xff),
            highlight_border: Color::rgba(0xff, 0xff, 0xff, 0xff),
            accent: Color::rgba(0x8b, 0xe0, 0xd0, 0xff),
            earned: Color::rgba(0xd9, 0xb3, 0x5c, 0xff),
            track: Color::rgba(0x2d, 0x32, 0x3a, 0xff),
        }
    }
}

/// How often a menu screen with nothing moving still wants a frame, when
/// the game needs ticks anyway (music to advance, a gamepad to poll: hosts
/// do not wake on gamepad input).
pub const MENU_TICK: Duration = Duration::from_millis(100);

/// Frame cadence while something on a menu moves (a scroll glide, a held
/// stick repeating).
pub const MENU_ANIMATION_FRAME: Duration = Duration::from_millis(16);

/// The frame demand for a menu screen.
///
/// `animating` comes from the screen's own `is_animating`. `needs_ticks` is
/// the game's: true while music plays or a gamepad is connected. Input
/// always wakes an idle or ticking menu at once.
#[must_use]
pub fn menu_frame_demand(animating: bool, needs_ticks: bool) -> FrameDemand {
    if animating {
        FrameDemand::after(MENU_ANIMATION_FRAME)
    } else if needs_ticks {
        FrameDemand::idle_until(MENU_TICK)
    } else {
        FrameDemand::Idle
    }
}

/// The part of the surface not under a system bar.
pub(crate) fn safe_rect(surface: SurfaceInfo, insets: SafeAreaInsets) -> Rect {
    Rect {
        x: insets.left,
        y: insets.top,
        width: (surface.width - insets.left - insets.right).max(0.0),
        height: (surface.height - insets.top - insets.bottom).max(0.0),
    }
}

#[cfg(test)]
pub(crate) mod test_input {
    use loadngo_host_core::{
        GamepadButton, GamepadSnapshot, HostKey, HostKeyEvent, InputSnapshot, SafeAreaInsets,
        SurfaceInfo, TouchPhase, TouchPoint,
    };

    pub const PHONE: SurfaceInfo = SurfaceInfo {
        width: 390.0,
        height: 844.0,
    };
    pub const LANDSCAPE_PHONE: SurfaceInfo = SurfaceInfo {
        width: 844.0,
        height: 390.0,
    };
    pub const DESKTOP: SurfaceInfo = SurfaceInfo {
        width: 1280.0,
        height: 800.0,
    };
    pub const NO_INSETS: SafeAreaInsets = SafeAreaInsets {
        left: 0.0,
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
    };

    pub fn blank() -> InputSnapshot {
        InputSnapshot::default()
    }

    /// A key press as the hosts report it: an event, the key held, and the
    /// legacy flag `InputSnapshot::key_pressed` reads for some keys.
    pub fn key(key: HostKey) -> InputSnapshot {
        InputSnapshot {
            key_events: vec![HostKeyEvent {
                key,
                modifiers: ui_core::Modifiers::default(),
            }],
            keys_down: vec![key],
            escape_pressed: key == HostKey::Escape,
            space_pressed: key == HostKey::Space,
            space_down: key == HostKey::Space,
            up_pressed: key == HostKey::Up,
            down_pressed: key == HostKey::Down,
            ..InputSnapshot::default()
        }
    }

    pub fn pad(pressed: GamepadButton) -> InputSnapshot {
        InputSnapshot {
            gamepads: vec![GamepadSnapshot {
                connected: true,
                buttons_down: vec![pressed],
                buttons_pressed: vec![pressed],
                ..GamepadSnapshot::cleared(0)
            }],
            ..InputSnapshot::default()
        }
    }

    pub fn pad_idle() -> InputSnapshot {
        InputSnapshot {
            gamepads: vec![GamepadSnapshot {
                connected: true,
                ..GamepadSnapshot::cleared(0)
            }],
            ..InputSnapshot::default()
        }
    }

    pub fn touch(x: f32, y: f32, phase: TouchPhase) -> InputSnapshot {
        let mut input = InputSnapshot {
            mouse_x: x,
            mouse_y: y,
            ..InputSnapshot::default()
        };
        input.touches[0] = Some(TouchPoint { id: 7, x, y, phase });
        input
    }

    pub fn click(x: f32, y: f32, pressed: bool) -> InputSnapshot {
        InputSnapshot {
            mouse_x: x,
            mouse_y: y,
            mouse_pressed: pressed,
            mouse_down: pressed,
            mouse_released: !pressed,
            ..InputSnapshot::default()
        }
    }
}
