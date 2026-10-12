//! Turns one frame's `InputSnapshot` into what the menu screens act on:
//! pointer events for buttons and sliders, discrete navigation steps, and
//! which input the player is using right now.

use loadngo_host_core::{GamepadButton, HostKey, InputSnapshot, TouchPhase};
use loadngo_touch::{NavRepeat, NAV_STICK_DEADZONE};
use ui_core::{
    ButtonModel, Modifiers, NavDirection, Point, PointerButton, PointerState, SliderModel, UiEvent,
    WidgetAction,
};

/// Feeds one frame's mouse and touch input into `button` and reports
/// whether it activated (pressed, then released over it).
///
/// `PointerMoved` is sent every frame so hover stays live under a still
/// mouse. A finger that lifts also gets `PointerLeft`: phones report the
/// last touch as the mouse position, so without it the button would stay
/// hovered after a tap.
pub(crate) fn button_activated(button: &mut ButtonModel, input: &InputSnapshot) -> bool {
    let mut activated = false;
    let mouse = mouse_state(input);
    button.handle_event(UiEvent::PointerMoved(mouse));
    if input.mouse_pressed {
        activated |= activates(button.handle_event(UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state: mouse,
        }));
    }
    if input.mouse_released {
        activated |= activates(button.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: mouse,
        }));
    }
    for touch in input.active_touches() {
        let pointer = PointerState::touch(
            touch.id,
            Point {
                x: touch.x,
                y: touch.y,
            },
        );
        let response = match touch.phase {
            TouchPhase::Started => button.handle_event(UiEvent::PointerPressed {
                button: PointerButton::Primary,
                state: pointer,
            }),
            TouchPhase::Moved | TouchPhase::Stationary => {
                button.handle_event(UiEvent::PointerMoved(pointer))
            }
            TouchPhase::Ended => {
                let released = button.handle_event(UiEvent::PointerReleased {
                    button: PointerButton::Primary,
                    state: pointer,
                });
                button.handle_event(UiEvent::PointerLeft);
                released
            }
            TouchPhase::Cancelled => button.handle_event(UiEvent::PointerLeft),
        };
        activated |= activates(response);
    }
    activated
}

fn activates(response: ui_core::WidgetResponse) -> bool {
    matches!(response.action, Some(WidgetAction::Activate(_)))
}

/// Feeds one frame's mouse and touch input into `slider` and reports
/// whether its value changed.
pub(crate) fn slider_changed(slider: &mut SliderModel, input: &InputSnapshot) -> bool {
    let before = slider.value;
    let mouse = mouse_state(input);
    slider.handle_event(UiEvent::PointerMoved(mouse));
    if input.mouse_pressed {
        slider.handle_event(UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state: mouse,
        });
    }
    if input.mouse_released {
        slider.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: mouse,
        });
    }
    for touch in input.active_touches() {
        let pointer = PointerState::touch(
            touch.id,
            Point {
                x: touch.x,
                y: touch.y,
            },
        );
        match touch.phase {
            TouchPhase::Started => {
                slider.handle_event(UiEvent::PointerPressed {
                    button: PointerButton::Primary,
                    state: pointer,
                });
            }
            TouchPhase::Moved | TouchPhase::Stationary => {
                slider.handle_event(UiEvent::PointerMoved(pointer));
            }
            TouchPhase::Ended => {
                slider.handle_event(UiEvent::PointerReleased {
                    button: PointerButton::Primary,
                    state: pointer,
                });
                slider.handle_event(UiEvent::PointerLeft);
            }
            TouchPhase::Cancelled => {
                slider.handle_event(UiEvent::PointerLeft);
            }
        }
    }
    (slider.value - before).abs() > f32::EPSILON
}

fn mouse_state(input: &InputSnapshot) -> PointerState {
    PointerState::mouse(
        Point {
            x: input.mouse_x,
            y: input.mouse_y,
        },
        Modifiers::default(),
    )
}

pub(crate) fn gamepad_pressed(input: &InputSnapshot, button: GamepadButton) -> bool {
    input
        .primary_gamepad()
        .is_some_and(|gamepad| gamepad.button_pressed(button))
}

fn gamepad_down(input: &InputSnapshot, button: GamepadButton) -> bool {
    input
        .primary_gamepad()
        .is_some_and(|gamepad| gamepad.button_down(button))
}

/// Confirm: Enter or Space on a keyboard, South on a gamepad.
pub(crate) fn confirm_pressed(input: &InputSnapshot) -> bool {
    input.key_pressed(HostKey::Enter)
        || input.key_pressed(HostKey::Space)
        || gamepad_pressed(input, GamepadButton::South)
}

/// Back: Escape on a keyboard, East on a gamepad.
pub(crate) fn back_pressed(input: &InputSnapshot) -> bool {
    input.key_pressed(HostKey::Escape) || gamepad_pressed(input, GamepadButton::East)
}

/// Whether every key and button a menu screen acts on is released, and the
/// stick is centred.
///
/// A screen starts disarmed and arms only once this holds, so a press that
/// opened it (still held on its first frame) cannot also act on it. This is
/// the release-before-arm rule `ui_core::FocusRing` documents.
pub(crate) fn menu_controls_released(input: &InputSnapshot) -> bool {
    let keys = [
        HostKey::Enter,
        HostKey::Space,
        HostKey::Escape,
        HostKey::Up,
        HostKey::Down,
        HostKey::Left,
        HostKey::Right,
    ];
    if keys.into_iter().any(|key| input.key_down(key)) {
        return false;
    }
    let Some(gamepad) = input.primary_gamepad() else {
        return true;
    };
    let buttons = [
        GamepadButton::South,
        GamepadButton::East,
        GamepadButton::DPadUp,
        GamepadButton::DPadDown,
        GamepadButton::DPadLeft,
        GamepadButton::DPadRight,
    ];
    if buttons
        .into_iter()
        .any(|button| gamepad_down(input, button))
    {
        return false;
    }
    let stick = gamepad.left_stick.with_deadzone(NAV_STICK_DEADZONE);
    stick.x.abs() < f32::EPSILON && stick.y.abs() < f32::EPSILON
}

/// Which input the player last used, deciding what a screen highlights:
/// the focused control for keys and pads, the control under the pointer
/// for a mouse or finger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pointing {
    Directional,
    Pointer,
}

/// Directional navigation from arrow keys and the gamepad, with the
/// gamepad's hold-to-repeat from `loadngo_touch::NavRepeat`. Arrow keys
/// step once per key event, so the OS key repeat applies to them.
#[derive(Debug, Clone, Default)]
pub(crate) struct MenuNav {
    repeat: NavRepeat,
    last_mouse: Option<(f32, f32)>,
}

impl MenuNav {
    pub(crate) fn reset(&mut self) {
        self.repeat.reset();
    }

    /// Whether a direction is held on the gamepad, so repeats are due and
    /// the caller needs frames on a steady cadence.
    pub(crate) fn repeating(&self) -> bool {
        self.repeat.held().is_some()
    }

    pub(crate) fn step(
        &mut self,
        input: &InputSnapshot,
        delta_seconds: f32,
    ) -> Option<NavDirection> {
        let pad = self.repeat.update(input, delta_seconds);
        if pad.is_some() {
            return pad;
        }
        [
            (HostKey::Up, NavDirection::Up),
            (HostKey::Down, NavDirection::Down),
            (HostKey::Left, NavDirection::Left),
            (HostKey::Right, NavDirection::Right),
        ]
        .into_iter()
        .find_map(|(key, direction)| input.key_pressed(key).then_some(direction))
    }

    /// Updates `pointing` from this frame: a moved or clicked mouse or a
    /// new touch means the pointer; navigation or confirm keys and gamepad
    /// buttons mean directional.
    pub(crate) fn update_pointing(&mut self, input: &InputSnapshot, pointing: &mut Pointing) {
        let mouse = (input.mouse_x, input.mouse_y);
        let moved = self.last_mouse.is_some_and(|last| last != mouse);
        self.last_mouse = Some(mouse);
        let touched = input
            .active_touches()
            .any(|touch| touch.phase == TouchPhase::Started);
        if moved || input.mouse_pressed || touched {
            *pointing = Pointing::Pointer;
        }
        let keys = [
            HostKey::Up,
            HostKey::Down,
            HostKey::Left,
            HostKey::Right,
            HostKey::Enter,
        ];
        let pad = input
            .primary_gamepad()
            .is_some_and(|gamepad| !gamepad.buttons_pressed.is_empty())
            || self.repeat.held().is_some();
        if pad || keys.into_iter().any(|key| input.key_pressed(key)) {
            *pointing = Pointing::Directional;
        }
    }
}
