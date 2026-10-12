//! The sound settings panel: one slider per channel the game offers, and a
//! close button.

use loadngo_host_core::{
    GamepadButton, InputSnapshot, RenderOp, RenderTextHorizontalAlign, SafeAreaInsets, SurfaceInfo,
};
use ui_core::{
    ButtonModel, FocusEntry, FocusMove, FocusRing, NavDirection, Rect, SliderModel, UiEvent,
    WidgetId,
};

use crate::input::{
    back_pressed, button_activated, confirm_pressed, gamepad_pressed, menu_controls_released,
    slider_changed, MenuNav, Pointing,
};
use crate::paint::{
    centred_panel, close_button_rect, push_button_frame, push_fill, push_panel, push_text,
    PANEL_HEADER, PANEL_PADDING,
};
use crate::{safe_rect, MenuTheme};

/// A volume the panel can adjust. The game maps each to its mixer
/// (`AudioMixer::set_master_volume`, `set_bus_volume`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundChannel {
    Master,
    Music,
    Effects,
}

/// One slider: the channel, its localized label and its current volume.
/// Volumes run from 0 to `max` (2.0 for `AudioPreferences`); the panel
/// shows them as a percentage of that range.
#[derive(Debug, Clone, PartialEq)]
pub struct SoundLevel {
    pub channel: SoundChannel,
    pub label: String,
    pub volume: f32,
    pub max: f32,
}

/// What a frame on the panel did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SoundPanelResponse {
    /// Channels whose volume changed this frame, with the new volume. Apply
    /// them to the mixer at once so the player hears the change.
    pub changed: Vec<(SoundChannel, f32)>,
    /// The player closed the panel. Save the preferences now, once, rather
    /// than on every change.
    pub closed: bool,
}

const MAX_WIDTH: f32 = 560.0;
const ROW_HEIGHT: f32 = 72.0;
const SLIDER_HEIGHT: f32 = 44.0;
/// One key press or pad step moves a slider by this share of its range.
const STEP_FRACTION: f32 = 0.05;
const CLOSE_ID: WidgetId = WidgetId(200);

struct Row {
    channel: SoundChannel,
    slider: SliderModel,
}

/// The sound panel's state, kept by the game while the panel is open.
pub struct SoundPanel {
    title: String,
    rows: Vec<Row>,
    close: ButtonModel,
    focus: FocusRing,
    nav: MenuNav,
    pointing: Pointing,
}

impl SoundPanel {
    /// Opens the panel on the current volumes. Make a new one each time the
    /// panel opens, so it starts from what the mixer has now.
    #[must_use]
    pub fn new(title: impl Into<String>, levels: Vec<SoundLevel>) -> Self {
        let rows = levels
            .into_iter()
            .enumerate()
            .map(|(index, level)| Row {
                channel: level.channel,
                slider: SliderModel::with_id(
                    WidgetId(201 + index as u64),
                    level.label,
                    Rect::default(),
                    0.0,
                    level.max,
                    level.volume,
                ),
            })
            .collect();
        let mut panel = Self {
            title: title.into(),
            rows,
            close: ButtonModel::with_id(CLOSE_ID, "X", Rect::default()),
            focus: FocusRing::new(),
            nav: MenuNav::default(),
            pointing: Pointing::Directional,
        };
        panel.layout(Rect::default());
        if let Some(first) = panel.rows.first().map(|row| row.slider.widget_id) {
            let moved = panel.focus.focus(first);
            panel.apply_focus_move(moved);
        }
        panel
    }

    /// One frame of input.
    pub fn update(
        &mut self,
        input: &InputSnapshot,
        surface: SurfaceInfo,
        insets: SafeAreaInsets,
        delta_seconds: f32,
    ) -> SoundPanelResponse {
        let mut response = SoundPanelResponse::default();
        let panel = self.panel_rect(safe_rect(surface, insets));
        self.layout(panel);

        if button_activated(&mut self.close, input) {
            response.closed = true;
            return response;
        }
        for row in &mut self.rows {
            if slider_changed(&mut row.slider, input) {
                response.changed.push((row.channel, row.slider.value));
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
            return response;
        }

        if back_pressed(input) {
            response.closed = true;
            return response;
        }
        if let Some(direction) = step {
            if let Some(change) = self.adjust_focused(direction) {
                response.changed.push(change);
            } else {
                let moved = self.focus.navigate(direction);
                self.apply_focus_move(moved);
            }
        }
        // Confirm on Close closes. Space anywhere closes too, as it always
        // has in sng-roguelite; South only on Close, because on a slider it
        // would surprise a player reaching for "accept".
        let on_close = self.focus.focused_id() == Some(CLOSE_ID);
        if (on_close && confirm_pressed(input))
            || input.key_pressed(loadngo_host_core::HostKey::Space)
            || (on_close && gamepad_pressed(input, GamepadButton::South))
        {
            response.closed = true;
        }
        response
    }

    /// Whether frames are needed on a steady cadence (a held stick).
    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.nav.repeating()
    }

    /// Appends the panel over whatever the game drew.
    pub fn paint(
        &self,
        ops: &mut Vec<RenderOp>,
        theme: &MenuTheme,
        surface: SurfaceInfo,
        insets: SafeAreaInsets,
    ) {
        let panel = self.panel_rect(safe_rect(surface, insets));
        let whole = Rect {
            x: 0.0,
            y: 0.0,
            width: surface.width,
            height: surface.height,
        };
        push_panel(
            ops,
            theme,
            whole,
            panel,
            &self.title,
            self.highlighted(self.close.focused, self.close.hover, self.close.pressed),
        );
        for row in &self.rows {
            let slider = &row.slider;
            let highlighted = self.highlighted(slider.focused, slider.hover, slider.dragging);
            push_slider(ops, theme, slider, highlighted);
        }
    }

    fn highlighted(&self, focused: bool, hover: bool, pressed: bool) -> bool {
        pressed
            || match self.pointing {
                Pointing::Pointer => hover,
                Pointing::Directional => focused,
            }
    }

    fn panel_rect(&self, safe: Rect) -> Rect {
        let wanted = PANEL_HEADER + 20.0 + ROW_HEIGHT * self.rows.len() as f32 + 12.0;
        centred_panel(safe, MAX_WIDTH, wanted)
    }

    fn layout(&mut self, panel: Rect) {
        let close = close_button_rect(panel);
        self.close.set_bounds(close);
        let mut entries = Vec::with_capacity(self.rows.len() + 1);
        entries.push(FocusEntry::new(CLOSE_ID, close));
        for (index, row) in self.rows.iter_mut().enumerate() {
            let bounds = Rect {
                x: panel.x + PANEL_PADDING,
                y: panel.y + PANEL_HEADER + 20.0 + ROW_HEIGHT * index as f32,
                width: (panel.width - PANEL_PADDING * 2.0).max(0.0),
                height: SLIDER_HEIGHT,
            };
            row.slider.set_bounds(bounds);
            entries.push(FocusEntry::new(row.slider.widget_id, bounds));
        }
        self.focus.set_entries(entries);
    }

    /// Left/Right on a focused slider move it by one step. Returns the
    /// change, or `None` if the direction should move focus instead.
    fn adjust_focused(&mut self, direction: NavDirection) -> Option<(SoundChannel, f32)> {
        let sign = match direction {
            NavDirection::Left => -1.0,
            NavDirection::Right => 1.0,
            NavDirection::Up | NavDirection::Down => return None,
        };
        let focused = self.focus.focused_id()?;
        let row = self
            .rows
            .iter_mut()
            .find(|row| row.slider.widget_id == focused)?;
        let slider = &mut row.slider;
        let before = slider.value;
        slider.set_value(before + sign * (slider.max - slider.min) * STEP_FRACTION);
        ((slider.value - before).abs() > f32::EPSILON).then_some((row.channel, slider.value))
    }

    fn apply_focus_move(&mut self, moved: Option<FocusMove>) {
        let Some(moved) = moved else {
            return;
        };
        for (id, focused) in moved
            .lost
            .map(|id| (id, false))
            .into_iter()
            .chain([(moved.gained, true)])
        {
            let event = UiEvent::FocusChanged(focused);
            if id == CLOSE_ID {
                self.close.handle_event(event);
            } else if let Some(row) = self.rows.iter_mut().find(|row| row.slider.widget_id == id) {
                row.slider.handle_event(event);
            }
        }
    }
}

fn push_slider(
    ops: &mut Vec<RenderOp>,
    theme: &MenuTheme,
    slider: &SliderModel,
    highlighted: bool,
) {
    let range = (slider.max - slider.min).max(0.0001);
    let fraction = ((slider.value - slider.min) / range).clamp(0.0, 1.0);
    // `fraction` is within 0..=1, so this is 0..=100.
    #[allow(clippy::cast_possible_truncation)]
    let percent = (fraction * 100.0).round() as i32;
    let rect = slider.bounds;
    push_text(
        ops,
        Rect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: 20.0,
        },
        &format!("{}  {percent}%", slider.text),
        theme.text,
        16,
        RenderTextHorizontalAlign::Left,
    );
    let track = Rect {
        x: rect.x,
        y: rect.y + 24.0,
        width: rect.width,
        height: rect.height - 24.0,
    };
    push_button_frame(ops, theme, track, highlighted);
    push_fill(
        ops,
        Rect {
            width: track.width * fraction,
            ..track
        },
        theme.accent,
    );
}

#[cfg(test)]
mod tests {
    use loadngo_host_core::{GamepadButton, HostKey, InputSnapshot, TouchPhase};

    use super::{SoundChannel, SoundLevel, SoundPanel};
    use crate::test_input::{blank, click, key, pad, pad_idle, touch, DESKTOP, NO_INSETS, PHONE};

    fn panel() -> SoundPanel {
        SoundPanel::new(
            "Sound",
            vec![
                SoundLevel {
                    channel: SoundChannel::Master,
                    label: "Master".to_string(),
                    volume: 1.0,
                    max: 2.0,
                },
                SoundLevel {
                    channel: SoundChannel::Music,
                    label: "Music".to_string(),
                    volume: 1.0,
                    max: 2.0,
                },
            ],
        )
    }

    fn frame(panel: &mut SoundPanel, input: &InputSnapshot) -> super::SoundPanelResponse {
        panel.update(input, DESKTOP, NO_INSETS, 0.016)
    }

    #[test]
    fn escape_closes_once_armed() {
        let mut panel = panel();
        assert!(!frame(&mut panel, &key(HostKey::Escape)).closed);
        frame(&mut panel, &blank());
        assert!(frame(&mut panel, &key(HostKey::Escape)).closed);
    }

    #[test]
    fn right_arrow_raises_the_focused_slider_by_one_step() {
        let mut panel = panel();
        frame(&mut panel, &blank());
        let response = frame(&mut panel, &key(HostKey::Right));
        assert_eq!(response.changed.len(), 1);
        let (channel, volume) = response.changed[0];
        assert_eq!(channel, SoundChannel::Master);
        assert!((volume - 1.1).abs() < 1e-4, "{volume}");
    }

    #[test]
    fn down_moves_to_the_next_slider() {
        let mut panel = panel();
        frame(&mut panel, &blank());
        frame(&mut panel, &key(HostKey::Down));
        let response = frame(&mut panel, &key(HostKey::Left));
        assert_eq!(response.changed[0].0, SoundChannel::Music);
    }

    #[test]
    fn gamepad_east_closes_and_south_does_not_on_a_slider() {
        let mut panel = panel();
        frame(&mut panel, &pad_idle());
        assert!(!frame(&mut panel, &pad(GamepadButton::South)).closed);
        frame(&mut panel, &pad_idle());
        assert!(frame(&mut panel, &pad(GamepadButton::East)).closed);
    }

    #[test]
    fn tapping_close_closes() {
        let mut panel = panel();
        panel.update(&blank(), PHONE, NO_INSETS, 0.016);
        let close = panel.close.bounds;
        let (x, y) = (close.x + 10.0, close.y + 10.0);
        panel.update(&touch(x, y, TouchPhase::Started), PHONE, NO_INSETS, 0.016);
        assert!(
            panel
                .update(&touch(x, y, TouchPhase::Ended), PHONE, NO_INSETS, 0.016)
                .closed
        );
    }

    #[test]
    fn clicking_a_track_sets_the_volume_there() {
        let mut panel = panel();
        frame(&mut panel, &blank());
        let track = panel.rows[1].slider.bounds;
        let x = track.x + track.width * 0.25;
        let y = track.y + 30.0;
        let response = frame(&mut panel, &click(x, y, true));
        assert_eq!(response.changed[0].0, SoundChannel::Music);
        assert!((response.changed[0].1 - 0.5).abs() < 0.05);
    }
}
