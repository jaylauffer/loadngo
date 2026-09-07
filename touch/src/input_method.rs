use loadngo_host_core::{InputSnapshot, PointF};

/// Deadzone applied when deciding whether the gamepad's sticks/triggers
/// count as "in use" this frame -- matches the deadzone both
/// `sng-roguelite` and `sng-zhoenus` already independently chose for their
/// own gamepad movement/steering, kept here too so "is the pad active"
/// agrees with "is the pad meaningfully deflected" everywhere.
const ACTIVITY_DEADZONE: f32 = 0.15;

/// Which input source most recently drove the player's input, between the
/// two shapes a desktop session can alternate between freely within one
/// sitting: keyboard+mouse, or a connected gamepad.
///
/// Deliberately not a `FormFactor` variant -- see `loadngo/docs/
/// GAMEPAD_INPUT.md`'s "input-source transitions" section, the design
/// problem this type resolves. `FormFactor`'s one-way latch (`Desktop ->
/// MobileTouch`, never demoted) fits touch, which a device either never or
/// permanently proves capable of. It doesn't fit a desktop player, who
/// legitimately picks up a mouse one moment and a controller the next --
/// forcing that into the same one-way-latch shape would leave a player
/// stuck seeing gamepad prompts forever after their first button press,
/// even after they set the controller down and grabbed the mouse. This
/// type is bidirectional instead: whichever source produced real input
/// most recently wins, and either side stays as long as the other stays
/// quiet.
///
/// Not meaningful on a touch device -- callers only need to consult this
/// when `FormFactor` is `Desktop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMethod {
    KeyboardMouse,
    Gamepad,
}

impl InputMethod {
    /// What a session starts as, before any input has been observed --
    /// keyboard/mouse, the same reasoning `FormFactor::platform_default`
    /// uses for defaulting to `Desktop`: nothing has proven a gamepad is
    /// even connected yet, and a session that ends before any input at all
    /// should show the more universally-available prompt.
    #[must_use]
    pub const fn platform_default() -> Self {
        Self::KeyboardMouse
    }

    /// Re-evaluates which source is active based on this frame's input.
    /// If both sources show real activity in the same frame (rare -- e.g.
    /// a held key while also holding a stick deflected), the previous
    /// value is kept rather than picking one arbitrarily, avoiding flicker
    /// on a genuinely ambiguous frame. Safe to call every frame regardless
    /// of whether either source actually produced input.
    pub fn update(&mut self, input: &InputSnapshot) {
        let keyboard_mouse_active = !input.keys_down.is_empty()
            || !input.key_events.is_empty()
            || input.mouse_down
            || input.mouse_pressed
            || input.mouse_released
            || input.mouse_wheel_x != 0.0
            || input.mouse_wheel_y != 0.0;
        let gamepad_active = input.gamepads.iter().any(|gamepad| {
            gamepad.connected
                && (!gamepad.buttons_down.is_empty()
                    || gamepad.left_stick.with_deadzone(ACTIVITY_DEADZONE) != PointF::default()
                    || gamepad.right_stick.with_deadzone(ACTIVITY_DEADZONE) != PointF::default()
                    || gamepad.left_trigger.raw > ACTIVITY_DEADZONE
                    || gamepad.right_trigger.raw > ACTIVITY_DEADZONE)
        });
        match (gamepad_active, keyboard_mouse_active) {
            (true, false) => *self = Self::Gamepad,
            (false, true) => *self = Self::KeyboardMouse,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::InputMethod;
    use loadngo_host_core::{GamepadButton, GamepadSnapshot, GamepadStick, InputSnapshot, PointF};

    fn blank_input() -> InputSnapshot {
        InputSnapshot::default()
    }

    fn connected_gamepad_pressing_south() -> GamepadSnapshot {
        GamepadSnapshot {
            connected: true,
            buttons_down: vec![GamepadButton::South],
            ..GamepadSnapshot::cleared(0)
        }
    }

    #[test]
    fn defaults_to_keyboard_mouse() {
        assert_eq!(InputMethod::platform_default(), InputMethod::KeyboardMouse);
    }

    #[test]
    fn a_gamepad_button_switches_to_gamepad() {
        let mut method = InputMethod::KeyboardMouse;
        let mut input = blank_input();
        input.gamepads = vec![connected_gamepad_pressing_south()];
        method.update(&input);
        assert_eq!(method, InputMethod::Gamepad);
    }

    #[test]
    fn a_deflected_stick_switches_to_gamepad() {
        let mut method = InputMethod::KeyboardMouse;
        let mut input = blank_input();
        input.gamepads = vec![GamepadSnapshot {
            connected: true,
            left_stick: GamepadStick {
                raw: PointF { x: 1.0, y: 0.0 },
            },
            ..GamepadSnapshot::cleared(0)
        }];
        method.update(&input);
        assert_eq!(method, InputMethod::Gamepad);
    }

    #[test]
    fn stick_noise_inside_the_deadzone_does_not_switch_to_gamepad() {
        let mut method = InputMethod::KeyboardMouse;
        let mut input = blank_input();
        input.gamepads = vec![GamepadSnapshot {
            connected: true,
            left_stick: GamepadStick {
                raw: PointF { x: 0.02, y: 0.0 },
            },
            ..GamepadSnapshot::cleared(0)
        }];
        method.update(&input);
        assert_eq!(method, InputMethod::KeyboardMouse);
    }

    #[test]
    fn a_held_key_switches_back_to_keyboard_mouse() {
        let mut method = InputMethod::Gamepad;
        let mut input = blank_input();
        input.keys_down = vec![loadngo_host_core::HostKey::W];
        method.update(&input);
        assert_eq!(method, InputMethod::KeyboardMouse);
    }

    #[test]
    fn a_mouse_click_switches_back_to_keyboard_mouse() {
        let mut method = InputMethod::Gamepad;
        let mut input = blank_input();
        input.mouse_pressed = true;
        method.update(&input);
        assert_eq!(method, InputMethod::KeyboardMouse);
    }

    #[test]
    fn no_input_this_frame_keeps_the_previous_method() {
        let mut method = InputMethod::Gamepad;
        method.update(&blank_input());
        assert_eq!(method, InputMethod::Gamepad);

        let mut method = InputMethod::KeyboardMouse;
        method.update(&blank_input());
        assert_eq!(method, InputMethod::KeyboardMouse);
    }

    #[test]
    fn simultaneous_activity_on_both_sources_keeps_the_previous_method() {
        let mut method = InputMethod::KeyboardMouse;
        let mut input = blank_input();
        input.keys_down = vec![loadngo_host_core::HostKey::W];
        input.gamepads = vec![connected_gamepad_pressing_south()];
        method.update(&input);
        assert_eq!(method, InputMethod::KeyboardMouse);
    }
}
