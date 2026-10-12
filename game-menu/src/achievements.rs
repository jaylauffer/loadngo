//! The achievements panel: a scrolling list of the game's achievements,
//! earned ones in the theme's `earned` colour.

use loadngo_host_core::{
    HostKey, InputSnapshot, RenderOp, RenderTextHorizontalAlign, SafeAreaInsets, SurfaceInfo,
    TouchPhase,
};
use ui_core::{ButtonModel, NavDirection, Point, Rect, ScrollRegionModel, WidgetId};

use crate::input::{
    back_pressed, button_activated, confirm_pressed, menu_controls_released, MenuNav,
};
use crate::paint::{
    centred_panel, close_button_rect, push_fill, push_panel, text_style, PANEL_HEADER,
    PANEL_PADDING,
};
use crate::{safe_rect, MenuTheme};

/// One line of the list, already formatted and localized by the game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AchievementRow {
    pub text: String,
    pub earned: bool,
}

/// The panel's fixed strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AchievementsText {
    pub title: String,
    /// Shown at the bottom on desktop, such as "Up / Down or wheel to
    /// scroll   Esc to close". `None` hides it (touch screens).
    pub hint: Option<String>,
}

const MAX_WIDTH: f32 = 760.0;
const MAX_HEIGHT: f32 = 560.0;
const ROW_HEIGHT: f32 = 30.0;
const ROW_FONT_SIZE: u16 = 14;
const HINT_HEIGHT: f32 = 30.0;
const CLOSE_ID: WidgetId = WidgetId(300);

#[derive(Debug, Clone, Copy)]
struct Drag {
    touch_id: u64,
    last_y: f32,
}

/// The achievements panel's state, kept by the game while it is open.
pub struct AchievementsPanel {
    scroll: ScrollRegionModel,
    drag: Option<Drag>,
    close: ButtonModel,
    nav: MenuNav,
    armed: bool,
}

impl Default for AchievementsPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl AchievementsPanel {
    /// A panel scrolled to the top. Make a new one each time it opens.
    #[must_use]
    pub fn new() -> Self {
        Self {
            scroll: ScrollRegionModel::new(Rect::default(), 0.0),
            drag: None,
            close: ButtonModel::with_id(CLOSE_ID, "X", Rect::default()),
            nav: MenuNav::default(),
            armed: false,
        }
    }

    /// One frame of input. `row_count` is the number of rows the game will
    /// paint. Returns whether the player closed the panel.
    pub fn update(
        &mut self,
        input: &InputSnapshot,
        surface: SurfaceInfo,
        insets: SafeAreaInsets,
        text: &AchievementsText,
        row_count: usize,
        delta_seconds: f32,
    ) -> bool {
        let panel = panel_rect(safe_rect(surface, insets));
        self.close.set_bounds(close_button_rect(panel));
        if button_activated(&mut self.close, input) {
            return true;
        }
        let viewport = list_viewport(panel, text.hint.is_some());
        self.scroll.set_viewport(viewport);
        self.scroll
            .set_content_height(ROW_HEIGHT * row_count as f32);

        self.apply_drag(input, viewport);
        if input.mouse_wheel_y != 0.0 {
            if input.mouse_wheel_precise {
                self.scroll.apply_scroll_delta(input.mouse_wheel_y);
            } else {
                self.scroll
                    .glide_scroll_delta(input.mouse_wheel_y * ROW_HEIGHT);
            }
        }

        let armed_before = self.armed;
        if !armed_before && menu_controls_released(input) {
            self.armed = true;
        }
        if armed_before {
            // The list has one control, Close, so directions scroll rather
            // than move focus, and confirm closes.
            match self.nav.step(input, delta_seconds) {
                Some(NavDirection::Down) => self.scroll.glide_scroll_delta(ROW_HEIGHT),
                Some(NavDirection::Up) => self.scroll.glide_scroll_delta(-ROW_HEIGHT),
                _ => {}
            }
            if input.key_pressed(HostKey::PageDown) {
                self.scroll.glide_scroll_delta(viewport.height);
            }
            if input.key_pressed(HostKey::PageUp) {
                self.scroll.glide_scroll_delta(-viewport.height);
            }
            if back_pressed(input) || confirm_pressed(input) {
                return true;
            }
        }
        self.scroll.advance(delta_seconds);
        false
    }

    /// Whether frames are needed on a steady cadence (a scroll still
    /// gliding, or a held stick).
    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.scroll.is_gliding() || self.nav.repeating()
    }

    /// Appends the panel over whatever the game drew.
    pub fn paint(
        &self,
        ops: &mut Vec<RenderOp>,
        theme: &MenuTheme,
        surface: SurfaceInfo,
        insets: SafeAreaInsets,
        text: &AchievementsText,
        rows: &[AchievementRow],
    ) {
        let panel = panel_rect(safe_rect(surface, insets));
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
            &text.title,
            self.close.hover || self.close.pressed,
        );

        let viewport = list_viewport(panel, text.hint.is_some());
        let origin = viewport.y - self.scroll.offset;
        let style = text_style(theme.text, ROW_FONT_SIZE, RenderTextHorizontalAlign::Left);
        ops.push(RenderOp::PushClip { rect: viewport });
        for (index, row) in rows.iter().enumerate() {
            let y = origin + ROW_HEIGHT * index as f32;
            if y + ROW_HEIGHT < viewport.y || y > viewport.y + viewport.height {
                continue;
            }
            let rect = Rect {
                x: panel.x + PANEL_PADDING,
                y,
                width: (panel.width - PANEL_PADDING * 2.0 - 14.0).max(0.0),
                height: ROW_HEIGHT,
            };
            if row.earned {
                let earned = loadngo_host_core::RenderTextStyle {
                    color: theme.earned,
                    ..style.clone()
                };
                // No bold weight in the renderer: the same text twice, a
                // fraction of a pixel apart, reads as bold.
                ops.push(RenderOp::Text {
                    rect,
                    text: row.text.clone(),
                    style: earned.clone(),
                });
                ops.push(RenderOp::Text {
                    rect: Rect {
                        x: rect.x + 0.6,
                        ..rect
                    },
                    text: row.text.clone(),
                    style: earned,
                });
            } else {
                ops.push(RenderOp::Text {
                    rect,
                    text: row.text.clone(),
                    style: style.clone(),
                });
            }
        }
        ops.push(RenderOp::PopClip);

        if let Some(track) = self.scroll.indicator_track_rect() {
            push_fill(ops, track, theme.track);
        }
        if let Some(thumb) = self.scroll.indicator_thumb_rect() {
            push_fill(ops, thumb, theme.accent);
        }
        if let Some(hint) = &text.hint {
            ops.push(RenderOp::Text {
                rect: Rect {
                    x: panel.x + PANEL_PADDING,
                    y: panel.y + panel.height - HINT_HEIGHT,
                    width: (panel.width - PANEL_PADDING * 2.0).max(0.0),
                    height: HINT_HEIGHT - 6.0,
                },
                text: hint.clone(),
                style: text_style(theme.muted_text, 13, RenderTextHorizontalAlign::Left),
            });
        }
    }

    /// A finger dragging the list scrolls it 1:1. The finger that started
    /// the drag keeps driving it while others come and go.
    fn apply_drag(&mut self, input: &InputSnapshot, viewport: Rect) {
        if let Some(drag) = self.drag {
            match input
                .active_touches()
                .find(|touch| touch.id == drag.touch_id)
            {
                Some(touch)
                    if matches!(touch.phase, TouchPhase::Moved | TouchPhase::Stationary) =>
                {
                    self.scroll.apply_scroll_delta(drag.last_y - touch.y);
                    self.drag = Some(Drag {
                        touch_id: drag.touch_id,
                        last_y: touch.y,
                    });
                }
                _ => self.drag = None,
            }
        } else if let Some(touch) = input.active_touches().find(|touch| {
            touch.phase == TouchPhase::Started
                && viewport.contains(Point {
                    x: touch.x,
                    y: touch.y,
                })
        }) {
            self.drag = Some(Drag {
                touch_id: touch.id,
                last_y: touch.y,
            });
        }
    }
}

fn panel_rect(safe: Rect) -> Rect {
    centred_panel(safe, MAX_WIDTH, MAX_HEIGHT)
}

fn list_viewport(panel: Rect, with_hint: bool) -> Rect {
    let top = panel.y + PANEL_HEADER + 10.0;
    let bottom = panel.y + panel.height - if with_hint { HINT_HEIGHT } else { 10.0 };
    Rect {
        x: panel.x,
        y: top,
        width: panel.width,
        height: (bottom - top).max(0.0),
    }
}

#[cfg(test)]
mod tests {
    use loadngo_host_core::{HostKey, InputSnapshot, RenderOp, TouchPhase};

    use super::{AchievementRow, AchievementsPanel, AchievementsText, ROW_HEIGHT};
    use crate::test_input::{blank, key, touch, DESKTOP, NO_INSETS, PHONE};
    use crate::MenuTheme;

    fn text() -> AchievementsText {
        AchievementsText {
            title: "Achievements".to_string(),
            hint: Some("hint".to_string()),
        }
    }

    fn frame(panel: &mut AchievementsPanel, input: &InputSnapshot, rows: usize) -> bool {
        panel.update(input, DESKTOP, NO_INSETS, &text(), rows, 0.016)
    }

    #[test]
    fn escape_closes_only_after_release() {
        let mut panel = AchievementsPanel::new();
        assert!(!frame(&mut panel, &key(HostKey::Escape), 40));
        assert!(!frame(&mut panel, &blank(), 40));
        assert!(frame(&mut panel, &key(HostKey::Escape), 40));
    }

    #[test]
    fn down_arrow_glides_a_row_and_then_settles() {
        let mut panel = AchievementsPanel::new();
        frame(&mut panel, &blank(), 40);
        frame(&mut panel, &key(HostKey::Down), 40);
        assert!(panel.is_animating());
        for _ in 0..120 {
            frame(&mut panel, &blank(), 40);
        }
        assert!(!panel.is_animating());
        assert!((panel.scroll.offset - ROW_HEIGHT).abs() < 0.01);
    }

    /// A trackpad reports pixels: scrolling by them 1:1, at once. Scaling
    /// them by a row height (as for wheel notches) made scrolling look
    /// line-by-line on a Mac (sng-roguelite, 2026-09).
    #[test]
    fn a_precise_wheel_delta_scrolls_by_pixels_at_once() {
        let mut panel = AchievementsPanel::new();
        let input = InputSnapshot {
            mouse_wheel_y: 7.0,
            mouse_wheel_precise: true,
            ..blank()
        };
        frame(&mut panel, &input, 40);
        assert!((panel.scroll.offset - 7.0).abs() < 0.01);
        assert!(!panel.is_animating());
    }

    /// A wheel notch eases one row into place rather than jumping (X11
    /// wheels report whole notches).
    #[test]
    fn a_wheel_notch_glides_one_row() {
        let mut panel = AchievementsPanel::new();
        let input = InputSnapshot {
            mouse_wheel_y: 1.0,
            ..blank()
        };
        frame(&mut panel, &input, 40);
        assert!(panel.scroll.offset > 0.0 && panel.scroll.offset < ROW_HEIGHT);
        for _ in 0..120 {
            frame(&mut panel, &blank(), 40);
        }
        assert!((panel.scroll.offset - ROW_HEIGHT).abs() < 0.01);
    }

    #[test]
    fn the_stick_scrolls_the_list_and_the_south_press_that_opened_it_does_not_close_it() {
        use crate::test_input::{pad, pad_idle};
        use loadngo_host_core::{GamepadButton, GamepadStick, PointF};

        let mut panel = AchievementsPanel::new();
        // The South press that opened the panel is still down on its first
        // frame: it must not close it.
        assert!(!frame(&mut panel, &pad(GamepadButton::South), 40));
        frame(&mut panel, &pad_idle(), 40);
        let mut stick = pad_idle();
        stick.gamepads[0].left_stick = GamepadStick {
            raw: PointF { x: 0.0, y: 1.0 },
        };
        frame(&mut panel, &stick, 40);
        for _ in 0..120 {
            frame(&mut panel, &pad_idle(), 40);
        }
        assert!((panel.scroll.offset - ROW_HEIGHT).abs() < 0.01);
        assert!(frame(&mut panel, &pad(GamepadButton::South), 40));
    }

    #[test]
    fn dragging_past_the_end_clamps() {
        let mut panel = AchievementsPanel::new();
        let rows = 30;
        panel.update(&blank(), PHONE, NO_INSETS, &text(), rows, 0.016);
        let viewport = panel.scroll.viewport;
        let (x, y) = (viewport.x + 40.0, viewport.y + viewport.height - 5.0);
        panel.update(
            &touch(x, y, TouchPhase::Started),
            PHONE,
            NO_INSETS,
            &text(),
            rows,
            0.016,
        );
        panel.update(
            &touch(x, y - 5000.0, TouchPhase::Moved),
            PHONE,
            NO_INSETS,
            &text(),
            rows,
            0.016,
        );
        let max = ROW_HEIGHT * rows as f32 - viewport.height;
        assert!(
            (panel.scroll.offset - max).abs() < 0.01,
            "{}",
            panel.scroll.offset
        );
    }

    #[test]
    fn a_finger_drag_scrolls_the_list() {
        let mut panel = AchievementsPanel::new();
        let rows = 60;
        panel.update(&blank(), PHONE, NO_INSETS, &text(), rows, 0.016);
        let viewport = panel.scroll.viewport;
        let x = viewport.x + 40.0;
        let y = viewport.y + 200.0;
        panel.update(
            &touch(x, y, TouchPhase::Started),
            PHONE,
            NO_INSETS,
            &text(),
            rows,
            0.016,
        );
        panel.update(
            &touch(x, y - 90.0, TouchPhase::Moved),
            PHONE,
            NO_INSETS,
            &text(),
            rows,
            0.016,
        );
        assert!((panel.scroll.offset - 90.0).abs() < 0.01);
    }

    #[test]
    fn rows_are_clipped_to_the_list_and_earned_rows_drawn_twice() {
        let mut panel = AchievementsPanel::new();
        let rows: Vec<AchievementRow> = (0..50)
            .map(|index| AchievementRow {
                text: format!("row {index}"),
                earned: index == 0,
            })
            .collect();
        frame(&mut panel, &blank(), rows.len());
        let mut ops = Vec::new();
        panel.paint(
            &mut ops,
            &MenuTheme::default(),
            DESKTOP,
            NO_INSETS,
            &text(),
            &rows,
        );
        let pushes = ops
            .iter()
            .filter(|op| matches!(op, RenderOp::PushClip { .. }))
            .count();
        let pops = ops
            .iter()
            .filter(|op| matches!(op, RenderOp::PopClip))
            .count();
        assert_eq!((pushes, pops), (1, 1));
        let texts: Vec<&str> = ops
            .iter()
            .filter_map(|op| match op {
                RenderOp::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts.iter().filter(|text| **text == "row 0").count(), 2);
        assert!(!texts.contains(&"row 49"), "off-screen rows are skipped");
    }
}
