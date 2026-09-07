//! Turns a gamepad's d-pad and left stick into discrete menu-navigation
//! steps, with hold-to-repeat timing.
//!
//! This is the layer between the host's raw `InputSnapshot` and `ui-core`'s
//! device-agnostic [`NavDirection`]: `ui-core` sits below `host-core` and so
//! can't see a `GamepadSnapshot` at all, while games shouldn't be
//! re-deriving repeat timing per screen. It lives here alongside
//! `FormFactor`/`InputMethod` for the same reason they do — this crate is
//! already where `InputSnapshot` gets translated into UI-level concepts.
//!
//! The timing is the whole point. A stick held toward a menu item is a
//! *continuous* signal; without repeat control it would step focus 60 times
//! a second and fly past everything. So a fresh deflection steps once
//! immediately, then nothing until [`HOLD_DELAY_SECONDS`], then a step every
//! [`REPEAT_INTERVAL_SECONDS`] — the standard menu cadence.
//!
//! Keyboard arrows are deliberately *not* folded in here yet: the games
//! consuming this keep their existing keyboard bindings unchanged, so there
//! is no caller for it. Add it when one exists.

use loadngo_host_core::{GamepadButton, InputSnapshot};
use ui_core::NavDirection;

/// Stick deflection past which the stick counts as pointing somewhere.
/// Matches the deadzone `sng-roguelite` and `sng-zhoenus` already use for
/// gameplay sticks, so a pad feels consistent between playing and
/// navigating menus.
pub const NAV_STICK_DEADZONE: f32 = 0.15;

/// How long a direction must be held before it starts repeating. Long
/// enough that deliberately tapping to move one item never double-steps.
pub const HOLD_DELAY_SECONDS: f32 = 0.45;

/// Gap between repeats once repeating has begun.
pub const REPEAT_INTERVAL_SECONDS: f32 = 0.12;

/// Converts held directional input into discrete navigation steps.
///
/// Call [`NavRepeat::update`] once per frame; it returns `Some(direction)`
/// on exactly the frames a step should happen.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct NavRepeat {
    held: Option<NavDirection>,
    held_seconds: f32,
    repeats: u16,
}

impl NavRepeat {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The direction currently being held, if any — for a caller that wants
    /// to reflect "the player is pushing this way" without acting on it.
    #[must_use]
    pub const fn held(&self) -> Option<NavDirection> {
        self.held
    }

    /// Forgets any held direction, so the next input counts as fresh.
    /// Worth calling when a screen opens, so a direction still held from
    /// the previous screen doesn't immediately repeat into the new one.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Advances by one frame, returning a navigation step if one is due.
    pub fn update(&mut self, input: &InputSnapshot, delta_seconds: f32) -> Option<NavDirection> {
        let direction = current_direction(input);
        let Some(direction) = direction else {
            // Released: the next deflection is a fresh step again.
            self.held = None;
            self.held_seconds = 0.0;
            self.repeats = 0;
            return None;
        };

        if self.held != Some(direction) {
            // A new direction (including reversing) always steps at once.
            self.held = Some(direction);
            self.held_seconds = 0.0;
            self.repeats = 0;
            return Some(direction);
        }

        self.held_seconds += delta_seconds.max(0.0);
        let due_at = HOLD_DELAY_SECONDS + REPEAT_INTERVAL_SECONDS * f32::from(self.repeats);
        if self.held_seconds >= due_at {
            self.repeats = self.repeats.saturating_add(1);
            return Some(direction);
        }
        None
    }
}

/// The direction the primary gamepad is currently pointing, from either the
/// d-pad or the left stick. The d-pad wins when both are engaged, being the
/// less ambiguous of the two.
fn current_direction(input: &InputSnapshot) -> Option<NavDirection> {
    let gamepad = input.primary_gamepad()?;

    let dpad = [
        (GamepadButton::DPadUp, NavDirection::Up),
        (GamepadButton::DPadDown, NavDirection::Down),
        (GamepadButton::DPadLeft, NavDirection::Left),
        (GamepadButton::DPadRight, NavDirection::Right),
    ]
    .into_iter()
    .find_map(|(button, direction)| gamepad.button_down(button).then_some(direction));
    if dpad.is_some() {
        return dpad;
    }

    // Dominant axis only: a diagonal push resolves to one direction rather
    // than stepping twice, matching how a d-pad behaves.
    let stick = gamepad.left_stick.with_deadzone(NAV_STICK_DEADZONE);
    if stick.x.abs() < f32::EPSILON && stick.y.abs() < f32::EPSILON {
        return None;
    }
    if stick.x.abs() >= stick.y.abs() {
        Some(if stick.x > 0.0 {
            NavDirection::Right
        } else {
            NavDirection::Left
        })
    } else {
        // Sticks are stored y-down (see `GamepadStick`), so pushing up is
        // negative y.
        Some(if stick.y > 0.0 {
            NavDirection::Down
        } else {
            NavDirection::Up
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{NavRepeat, HOLD_DELAY_SECONDS, REPEAT_INTERVAL_SECONDS};
    use loadngo_host_core::{GamepadButton, GamepadSnapshot, GamepadStick, InputSnapshot, PointF};
    use ui_core::NavDirection;

    fn blank_input() -> InputSnapshot {
        InputSnapshot {
            mouse_x: 0.0,
            mouse_y: 0.0,
            mouse_wheel_x: 0.0,
            mouse_wheel_y: 0.0,
            mouse_pressed: false,
            mouse_down: false,
            mouse_released: false,
            touches: [None; 8],
            escape_pressed: false,
            space_pressed: false,
            space_down: false,
            f3_pressed: false,
            r_pressed: false,
            up_pressed: false,
            down_pressed: false,
            modifiers: ui_core::Modifiers::default(),
            key_events: Vec::new(),
            keys_down: Vec::new(),
            typed_text: String::new(),
            gamepads: Vec::new(),
        }
    }

    fn with_stick(x: f32, y: f32) -> InputSnapshot {
        InputSnapshot {
            gamepads: vec![GamepadSnapshot {
                connected: true,
                left_stick: GamepadStick {
                    raw: PointF { x, y },
                },
                ..GamepadSnapshot::cleared(0)
            }],
            ..blank_input()
        }
    }

    fn with_dpad(button: GamepadButton) -> InputSnapshot {
        InputSnapshot {
            gamepads: vec![GamepadSnapshot {
                connected: true,
                buttons_down: vec![button],
                ..GamepadSnapshot::cleared(0)
            }],
            ..blank_input()
        }
    }

    #[test]
    fn a_fresh_stick_deflection_steps_once_immediately() {
        let mut nav = NavRepeat::new();
        let input = with_stick(1.0, 0.0);
        assert_eq!(nav.update(&input, 0.016), Some(NavDirection::Right));
        // Still held, but not long enough to repeat yet.
        assert_eq!(nav.update(&input, 0.016), None);
        assert_eq!(nav.update(&input, 0.016), None);
    }

    #[test]
    fn holding_repeats_only_after_the_delay_then_at_the_repeat_interval() {
        let mut nav = NavRepeat::new();
        let input = with_stick(0.0, 1.0);
        assert_eq!(nav.update(&input, 0.0), Some(NavDirection::Down));

        // Just short of the delay: still nothing.
        assert_eq!(nav.update(&input, HOLD_DELAY_SECONDS - 0.01), None);
        // Crossing the delay: one repeat.
        assert_eq!(nav.update(&input, 0.02), Some(NavDirection::Down));
        // Then one per interval.
        assert_eq!(
            nav.update(&input, REPEAT_INTERVAL_SECONDS),
            Some(NavDirection::Down)
        );
        assert_eq!(nav.update(&input, REPEAT_INTERVAL_SECONDS * 0.5), None);
        assert_eq!(
            nav.update(&input, REPEAT_INTERVAL_SECONDS * 0.5),
            Some(NavDirection::Down)
        );
    }

    #[test]
    fn returning_to_centre_re_arms_the_next_deflection() {
        let mut nav = NavRepeat::new();
        let deflected = with_stick(1.0, 0.0);
        assert_eq!(nav.update(&deflected, 0.016), Some(NavDirection::Right));
        assert_eq!(nav.update(&deflected, 0.016), None);

        // Centre.
        assert_eq!(nav.update(&with_stick(0.0, 0.0), 0.016), None);
        // Deflecting again steps immediately rather than waiting out a delay.
        assert_eq!(nav.update(&deflected, 0.016), Some(NavDirection::Right));
    }

    #[test]
    fn stick_noise_inside_the_deadzone_never_steps() {
        let mut nav = NavRepeat::new();
        assert_eq!(nav.update(&with_stick(0.05, 0.02), 0.016), None);
        assert_eq!(nav.held(), None);
    }

    #[test]
    fn a_diagonal_push_resolves_to_its_dominant_axis() {
        let mut nav = NavRepeat::new();
        // Mostly right, slightly up.
        assert_eq!(
            nav.update(&with_stick(0.9, -0.3), 0.016),
            Some(NavDirection::Right)
        );
        nav.reset();
        // Mostly up, slightly right.
        assert_eq!(
            nav.update(&with_stick(0.3, -0.9), 0.016),
            Some(NavDirection::Up)
        );
    }

    #[test]
    fn pushing_up_reads_as_up_despite_y_down_storage() {
        let mut nav = NavRepeat::new();
        // Sticks are stored y-down, so "up" is negative y -- the same
        // convention that was inverted in a real playtest bug once.
        assert_eq!(
            nav.update(&with_stick(0.0, -1.0), 0.016),
            Some(NavDirection::Up)
        );
    }

    #[test]
    fn the_dpad_navigates_too() {
        let mut nav = NavRepeat::new();
        assert_eq!(
            nav.update(&with_dpad(GamepadButton::DPadLeft), 0.016),
            Some(NavDirection::Left)
        );
    }

    #[test]
    fn reversing_direction_steps_immediately_without_waiting() {
        let mut nav = NavRepeat::new();
        assert_eq!(
            nav.update(&with_stick(1.0, 0.0), 0.016),
            Some(NavDirection::Right)
        );
        assert_eq!(
            nav.update(&with_stick(-1.0, 0.0), 0.016),
            Some(NavDirection::Left)
        );
    }

    #[test]
    fn no_gamepad_connected_never_steps() {
        let mut nav = NavRepeat::new();
        assert_eq!(nav.update(&blank_input(), 0.016), None);
    }
}
