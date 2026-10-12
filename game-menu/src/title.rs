//! The title menu: the game's name over a column of buttons, with Start as
//! the default choice.

use loadngo_host_core::{
    InputSnapshot, RenderOp, RenderTextHorizontalAlign, SafeAreaInsets, SurfaceInfo,
};
use ui_core::{ButtonModel, FocusEntry, FocusMove, FocusRing, Rect, UiEvent, WidgetId};

use crate::input::{button_activated, confirm_pressed, menu_controls_released, MenuNav, Pointing};
use crate::paint::{push_button_frame, push_fill, push_text};
use crate::{safe_rect, MenuTheme};

/// What the player chose on the title menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleAction {
    Start,
    Sound,
    Achievements,
    Quit,
}

const BUTTON_MAX_WIDTH: f32 = 340.0;
const BUTTON_HEIGHT: f32 = 56.0;
const BUTTON_MIN_HEIGHT: f32 = 44.0;
const BUTTON_GAP: f32 = 14.0;
const BUTTON_FONT_SIZE: u16 = 22;
const TITLE_HEIGHT: f32 = 72.0;
const TITLE_GAP: f32 = 36.0;
const SIDE_MARGIN: f32 = 24.0;
/// Room kept free above and below the menu on a tall screen.
const VERTICAL_MARGIN: f32 = 32.0;

/// Spacing of the title and the button column. A short screen (a phone
/// held sideways) gets the compact set so four buttons still fit.
#[derive(Debug, Clone, Copy)]
struct Spacing {
    title_height: f32,
    title_gap: f32,
    button_gap: f32,
    title_font_size: u16,
}

const REGULAR: Spacing = Spacing {
    title_height: TITLE_HEIGHT,
    title_gap: TITLE_GAP,
    button_gap: BUTTON_GAP,
    title_font_size: 48,
};

const COMPACT: Spacing = Spacing {
    title_height: 48.0,
    title_gap: 12.0,
    button_gap: 8.0,
    title_font_size: 32,
};

struct Entry {
    action: TitleAction,
    button: ButtonModel,
}

/// The title menu's state, kept by the game across frames.
pub struct TitleMenu {
    title: String,
    entries: Vec<Entry>,
    focus: FocusRing,
    nav: MenuNav,
    pointing: Pointing,
}

impl TitleMenu {
    /// A menu titled `title` offering `entries` (action and its localized
    /// label) top to bottom. The first entry is the default choice; put
    /// Start first.
    #[must_use]
    pub fn new(title: impl Into<String>, entries: Vec<(TitleAction, String)>) -> Self {
        let entries = entries
            .into_iter()
            .enumerate()
            .map(|(index, (action, label))| {
                let mut button = ButtonModel::with_id(widget_id(index), label, Rect::default());
                button.font_size = BUTTON_FONT_SIZE;
                Entry { action, button }
            })
            .collect();
        let mut menu = Self {
            title: title.into(),
            entries,
            focus: FocusRing::new(),
            nav: MenuNav::default(),
            pointing: Pointing::Directional,
        };
        menu.reset();
        menu
    }

    /// Returns the menu to its opening state, Start focused and disarmed.
    /// Call when the menu is shown again.
    pub fn reset(&mut self) {
        self.nav.reset();
        self.pointing = Pointing::Directional;
        self.focus.disarm();
        for entry in &mut self.entries {
            entry.button.hover = false;
            entry.button.pressed = false;
        }
        self.layout(Rect::default());
        if let Some(first) = self.entries.first().map(|entry| entry.button.widget_id) {
            let moved = self.focus.focus(first);
            self.apply_focus_move(moved);
        }
    }

    /// One frame of input. Returns the chosen action, if any.
    pub fn update(
        &mut self,
        input: &InputSnapshot,
        surface: SurfaceInfo,
        insets: SafeAreaInsets,
        delta_seconds: f32,
    ) -> Option<TitleAction> {
        self.layout(safe_rect(surface, insets));

        for entry in &mut self.entries {
            if button_activated(&mut entry.button, input) {
                return Some(entry.action);
            }
        }

        let armed_before = self.focus.is_armed();
        if !armed_before && menu_controls_released(input) {
            let moved = self.focus.arm();
            self.apply_focus_move(moved);
        }
        let step = if armed_before {
            self.nav.step(input, delta_seconds)
        } else {
            None
        };
        self.nav.update_pointing(input, &mut self.pointing);
        if !armed_before {
            return None;
        }
        if let Some(direction) = step {
            let moved = self.focus.navigate(direction);
            self.apply_focus_move(moved);
        }
        if confirm_pressed(input) {
            let focused = self.focus.focused_id()?;
            return self
                .entries
                .iter()
                .find(|entry| entry.button.widget_id == focused)
                .map(|entry| entry.action);
        }
        None
    }

    /// Whether frames are needed on a steady cadence (a held stick).
    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.nav.repeating()
    }

    /// Appends the menu: backdrop, title and buttons. A game that wants its
    /// own picture behind the menu can pass a theme with a transparent
    /// backdrop and draw first.
    pub fn paint(
        &self,
        ops: &mut Vec<RenderOp>,
        theme: &MenuTheme,
        surface: SurfaceInfo,
        insets: SafeAreaInsets,
    ) {
        push_fill(
            ops,
            Rect {
                x: 0.0,
                y: 0.0,
                width: surface.width,
                height: surface.height,
            },
            theme.backdrop,
        );
        let safe = safe_rect(surface, insets);
        let spacing = self.spacing(safe);
        let title_rect = self.title_rect(safe, spacing);
        let title_size = if safe.width < 480.0 {
            spacing.title_font_size.min(36)
        } else {
            spacing.title_font_size
        };
        push_text(
            ops,
            title_rect,
            &self.title,
            theme.title,
            title_size,
            RenderTextHorizontalAlign::Center,
        );
        for entry in &self.entries {
            let button = &entry.button;
            let highlighted = button.pressed
                || match self.pointing {
                    Pointing::Pointer => button.hover,
                    Pointing::Directional => button.focused,
                };
            push_button_frame(ops, theme, button.bounds, highlighted);
            push_text(
                ops,
                button.bounds,
                &button.text,
                theme.text,
                button.font_size,
                RenderTextHorizontalAlign::Center,
            );
        }
    }

    /// The button rects, top to bottom, for tests and for a game drawing
    /// decorations around them.
    #[must_use]
    pub fn button_rects(&self) -> Vec<Rect> {
        self.entries
            .iter()
            .map(|entry| entry.button.bounds)
            .collect()
    }

    fn spacing(&self, safe: Rect) -> Spacing {
        let count = self.entries.len().max(1) as f32;
        let regular = REGULAR.title_height
            + REGULAR.title_gap
            + count * BUTTON_MIN_HEIGHT
            + REGULAR.button_gap * (count - 1.0)
            + VERTICAL_MARGIN;
        if safe.height >= regular {
            REGULAR
        } else {
            COMPACT
        }
    }

    fn button_height(&self, safe: Rect, spacing: Spacing) -> f32 {
        let count = self.entries.len().max(1) as f32;
        let room = safe.height
            - spacing.title_height
            - spacing.title_gap
            - spacing.button_gap * (count - 1.0)
            - VERTICAL_MARGIN;
        (room / count).clamp(BUTTON_MIN_HEIGHT, BUTTON_HEIGHT)
    }

    fn stack_top(&self, safe: Rect, spacing: Spacing) -> f32 {
        let count = self.entries.len() as f32;
        let height = self.button_height(safe, spacing);
        let buttons = count * height + spacing.button_gap * (count - 1.0).max(0.0);
        let total = spacing.title_height + spacing.title_gap + buttons;
        safe.y + ((safe.height - total) * 0.5).max(0.0)
    }

    fn title_rect(&self, safe: Rect, spacing: Spacing) -> Rect {
        Rect {
            x: safe.x + SIDE_MARGIN,
            y: self.stack_top(safe, spacing),
            width: (safe.width - SIDE_MARGIN * 2.0).max(0.0),
            height: spacing.title_height,
        }
    }

    fn layout(&mut self, safe: Rect) {
        let spacing = self.spacing(safe);
        let width = (safe.width - SIDE_MARGIN * 2.0).clamp(0.0, BUTTON_MAX_WIDTH);
        let height = self.button_height(safe, spacing);
        let x = safe.x + (safe.width - width) * 0.5;
        let mut y = self.stack_top(safe, spacing) + spacing.title_height + spacing.title_gap;
        let mut focus_entries = Vec::with_capacity(self.entries.len());
        for entry in &mut self.entries {
            let bounds = Rect {
                x,
                y,
                width,
                height,
            };
            entry.button.set_bounds(bounds);
            focus_entries.push(FocusEntry::new(entry.button.widget_id, bounds));
            y += height + spacing.button_gap;
        }
        self.focus.set_entries(focus_entries);
    }

    fn apply_focus_move(&mut self, moved: Option<FocusMove>) {
        let Some(moved) = moved else {
            return;
        };
        for entry in &mut self.entries {
            let id = entry.button.widget_id;
            if Some(id) == moved.lost {
                entry.button.handle_event(UiEvent::FocusChanged(false));
            }
            if id == moved.gained {
                entry.button.handle_event(UiEvent::FocusChanged(true));
            }
        }
    }
}

fn widget_id(index: usize) -> WidgetId {
    WidgetId(100 + index as u64)
}

#[cfg(test)]
mod tests {
    use loadngo_host_core::{GamepadButton, HostKey, TouchPhase};

    use super::{TitleAction, TitleMenu};
    use crate::test_input::{
        blank, click, key, pad, pad_idle, touch, DESKTOP, LANDSCAPE_PHONE, NO_INSETS, PHONE,
    };

    fn full_menu() -> TitleMenu {
        TitleMenu::new(
            "Game",
            vec![
                (TitleAction::Start, "Start game".to_string()),
                (TitleAction::Sound, "Sound".to_string()),
                (TitleAction::Achievements, "Achievements".to_string()),
                (TitleAction::Quit, "Quit".to_string()),
            ],
        )
    }

    fn frame(
        menu: &mut TitleMenu,
        input: &loadngo_host_core::InputSnapshot,
    ) -> Option<TitleAction> {
        menu.update(input, DESKTOP, NO_INSETS, 0.016)
    }

    #[test]
    fn space_or_enter_starts_by_default_once_armed() {
        for confirm in [HostKey::Space, HostKey::Enter] {
            let mut menu = full_menu();
            assert_eq!(frame(&mut menu, &blank()), None);
            assert_eq!(frame(&mut menu, &key(confirm)), Some(TitleAction::Start));
        }
    }

    #[test]
    fn a_press_held_when_the_menu_opens_does_not_choose() {
        let mut menu = full_menu();
        assert_eq!(frame(&mut menu, &key(HostKey::Space)), None);
        assert_eq!(frame(&mut menu, &key(HostKey::Space)), None);
        assert_eq!(frame(&mut menu, &blank()), None);
        assert_eq!(
            frame(&mut menu, &key(HostKey::Space)),
            Some(TitleAction::Start)
        );
    }

    #[test]
    fn arrows_move_down_the_list_and_stop_at_the_end() {
        let mut menu = full_menu();
        frame(&mut menu, &blank());
        frame(&mut menu, &key(HostKey::Down));
        assert_eq!(
            frame(&mut menu, &key(HostKey::Enter)),
            Some(TitleAction::Sound)
        );
        for _ in 0..6 {
            frame(&mut menu, &key(HostKey::Down));
        }
        assert_eq!(
            frame(&mut menu, &key(HostKey::Enter)),
            Some(TitleAction::Quit)
        );
        frame(&mut menu, &key(HostKey::Up));
        assert_eq!(
            frame(&mut menu, &key(HostKey::Enter)),
            Some(TitleAction::Achievements)
        );
    }

    #[test]
    fn gamepad_navigates_and_confirms_with_south() {
        let mut menu = full_menu();
        frame(&mut menu, &pad_idle());
        frame(&mut menu, &pad(GamepadButton::DPadDown));
        frame(&mut menu, &pad_idle());
        frame(&mut menu, &pad(GamepadButton::DPadDown));
        frame(&mut menu, &pad_idle());
        assert_eq!(
            frame(&mut menu, &pad(GamepadButton::South)),
            Some(TitleAction::Achievements)
        );
    }

    #[test]
    fn a_tap_chooses_on_release_over_the_same_button() {
        let mut menu = full_menu();
        menu.update(&blank(), PHONE, NO_INSETS, 0.016);
        let sound = menu.button_rects()[1];
        let (x, y) = (sound.x + 10.0, sound.y + 10.0);
        assert_eq!(
            menu.update(&touch(x, y, TouchPhase::Started), PHONE, NO_INSETS, 0.016),
            None
        );
        assert_eq!(
            menu.update(&touch(x, y, TouchPhase::Ended), PHONE, NO_INSETS, 0.016),
            Some(TitleAction::Sound)
        );
    }

    #[test]
    fn a_tap_that_slides_off_the_button_chooses_nothing() {
        let mut menu = full_menu();
        menu.update(&blank(), PHONE, NO_INSETS, 0.016);
        let start = menu.button_rects()[0];
        menu.update(
            &touch(start.x + 5.0, start.y + 5.0, TouchPhase::Started),
            PHONE,
            NO_INSETS,
            0.016,
        );
        assert_eq!(
            menu.update(&touch(5.0, 5.0, TouchPhase::Ended), PHONE, NO_INSETS, 0.016),
            None
        );
    }

    #[test]
    fn a_mouse_click_chooses() {
        let mut menu = full_menu();
        frame(&mut menu, &blank());
        let quit = menu.button_rects()[3];
        let (x, y) = (quit.x + 20.0, quit.y + 20.0);
        assert_eq!(frame(&mut menu, &click(x, y, true)), None);
        assert_eq!(
            frame(&mut menu, &click(x, y, false)),
            Some(TitleAction::Quit)
        );
    }

    #[test]
    fn buttons_fit_inside_the_safe_area_on_phones_in_both_orientations() {
        for surface in [PHONE, LANDSCAPE_PHONE, DESKTOP] {
            let insets = loadngo_host_core::SafeAreaInsets {
                left: 0.0,
                top: 47.0,
                right: 0.0,
                bottom: 34.0,
            };
            let mut menu = full_menu();
            menu.update(&blank(), surface, insets, 0.016);
            let rects = menu.button_rects();
            for rect in &rects {
                assert!(rect.height >= 44.0, "{surface:?}: {rect:?}");
                assert!(rect.y >= insets.top, "{surface:?}: {rect:?}");
                assert!(
                    rect.y + rect.height <= surface.height - insets.bottom,
                    "{surface:?}: {rect:?}"
                );
                assert!(rect.x >= 0.0 && rect.x + rect.width <= surface.width);
            }
            for pair in rects.windows(2) {
                assert!(pair[0].y + pair[0].height < pair[1].y);
            }
        }
    }

    #[test]
    fn reset_returns_focus_to_start() {
        let mut menu = full_menu();
        frame(&mut menu, &blank());
        frame(&mut menu, &key(HostKey::Down));
        frame(&mut menu, &key(HostKey::Down));
        menu.reset();
        frame(&mut menu, &blank());
        assert_eq!(
            frame(&mut menu, &key(HostKey::Space)),
            Some(TitleAction::Start)
        );
    }
}
