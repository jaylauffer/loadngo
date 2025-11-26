use crate::{
    component::Component,
    geometry::{Color, Insets, Point, Rect},
    input::{Key, PointerButton, UiEvent},
    paint::{HorizontalAlign, PaintOp, Particle, TextLayoutMode, TextStyle, VerticalAlign},
    widget::{WidgetId, WidgetResponse},
};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq)]
pub struct ButtonModel {
    pub widget_id: WidgetId,
    pub bounds: Rect,
    pub text: String,
    pub font_size: u16,
    pub text_align: HorizontalAlign,
    pub hover: bool,
    pub pressed: bool,
    pub focused: bool,
    pulse_origin_seconds: Option<f64>,
}

impl ButtonModel {
    pub fn visual_outsets(&self) -> Insets {
        Self::interactive_visual_outsets_for_state(self.hover, self.pressed, self.focused)
    }

    pub fn interactive_visual_outsets() -> Insets {
        Self::interactive_visual_outsets_for_state(true, true, true)
    }

    fn interactive_visual_outsets_for_state(hover: bool, pressed: bool, focused: bool) -> Insets {
        if !hover && !pressed && !focused {
            return Insets::default();
        }

        // These outsets match the furthest extents painted by the hover/press trace and
        // particle accents around the button bounds.
        Insets {
            left: 2.0,
            top: 9.0,
            right: 6.0,
            bottom: 6.0,
        }
    }

    pub fn new(text: impl Into<String>, bounds: Rect) -> Self {
        Self::with_id(WidgetId(0), text, bounds)
    }

    pub fn with_id(widget_id: WidgetId, text: impl Into<String>, bounds: Rect) -> Self {
        Self {
            widget_id,
            bounds,
            text: text.into(),
            font_size: 18,
            text_align: HorizontalAlign::Center,
            hover: false,
            pressed: false,
            focused: false,
            pulse_origin_seconds: None,
        }
    }

    pub fn set_bounds(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    pub fn handle_event(&mut self, event: UiEvent) -> WidgetResponse {
        match event {
            UiEvent::PointerMoved(state) => {
                let hover = self.bounds.contains(state.position);
                if hover != self.hover {
                    self.hover = hover;
                    if hover {
                        self.restart_pulse_cycle();
                    } else {
                        self.clear_pulse_cycle_if_inactive();
                    }
                    return WidgetResponse::redraw();
                }
                WidgetResponse::default()
            }
            UiEvent::PointerLeft => {
                if self.hover {
                    self.hover = false;
                    self.clear_pulse_cycle_if_inactive();
                    return WidgetResponse::redraw();
                }
                WidgetResponse::default()
            }
            UiEvent::PointerPressed {
                button: PointerButton::Primary,
                state,
            } => {
                if self.bounds.contains(state.position) {
                    self.pressed = true;
                    self.restart_pulse_cycle();
                    return WidgetResponse::redraw_consumed();
                }
                WidgetResponse::default()
            }
            UiEvent::PointerReleased {
                button: PointerButton::Primary,
                state,
            } => {
                let was_pressed = self.pressed;
                self.pressed = false;
                self.clear_pulse_cycle_if_inactive();
                if was_pressed && self.bounds.contains(state.position) {
                    return WidgetResponse::activate(self.widget_id);
                }
                if was_pressed {
                    return WidgetResponse::redraw_consumed();
                }
                WidgetResponse::default()
            }
            UiEvent::FocusChanged(focused) => {
                if self.focused != focused {
                    self.focused = focused;
                    if focused {
                        self.restart_pulse_cycle();
                    } else {
                        self.clear_pulse_cycle_if_inactive();
                    }
                    return WidgetResponse::redraw();
                }
                WidgetResponse::default()
            }
            UiEvent::KeyPressed {
                key: Key::Enter | Key::Space,
                ..
            } => WidgetResponse::activate(self.widget_id),
            _ => WidgetResponse::default(),
        }
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        let text_inset_x = match self.text_align {
            HorizontalAlign::Left | HorizontalAlign::Right => 14.0,
            HorizontalAlign::Center => 0.0,
        };
        let pulse = if self.hover || self.focused || self.pressed {
            let elapsed_s = self
                .pulse_origin_seconds
                .map(animation_elapsed_seconds)
                .unwrap_or(0.0);
            Some(hover_pulse(elapsed_s, self.pressed))
        } else {
            None
        };
        let (fill, border) = if let Some(pulse) = pulse {
            if self.pressed {
                (
                    lerp_color(
                        Color::rgba(0x4c, 0x78, 0xbd, 0xf8),
                        Color::rgba(0xf3, 0xf9, 0xff, 0xff),
                        pulse,
                    ),
                    lerp_color(
                        Color::rgba(0x2d, 0x52, 0x90, 0xff),
                        Color::rgba(0xf0, 0xf9, 0xff, 0xff),
                        pulse,
                    ),
                )
            } else {
                (
                    lerp_color(
                        Color::rgba(0x5e, 0x84, 0xbd, 0xec),
                        Color::rgba(0xf8, 0xfc, 0xff, 0xff),
                        pulse,
                    ),
                    lerp_color(
                        Color::rgba(0x36, 0x5f, 0xa0, 0xf6),
                        Color::rgba(0xe1, 0xf1, 0xff, 0xff),
                        pulse,
                    ),
                )
            }
        } else {
            (
                Color::rgba(0xf0, 0xf0, 0xf0, 0xd8),
                Color::rgba(0x86, 0x8d, 0xa0, 0xd4),
            )
        };
        let text_color = Color::rgba(0x12, 0x12, 0x12, 0xff);
        scene.push(PaintOp::FillRect {
            rect: self.bounds,
            color: fill,
        });
        if let Some(pulse) = pulse {
            scene.push(PaintOp::FillRect {
                rect: Rect {
                    x: self.bounds.x + 1.0,
                    y: self.bounds.y + 1.0,
                    width: (self.bounds.width - 2.0).max(1.0),
                    height: (self.bounds.height - 2.0).max(1.0),
                },
                color: Color::rgba(
                    0xff,
                    0xff,
                    0xff,
                    if self.pressed {
                        (8.0 + pulse * 132.0).round().clamp(0.0, 255.0) as u8
                    } else {
                        (4.0 + pulse * 118.0).round().clamp(0.0, 255.0) as u8
                    },
                ),
            });
        }
        scene.push(PaintOp::StrokeRect {
            rect: self.bounds,
            color: border,
        });
        if let Some(pulse) = pulse {
            scene.push(PaintOp::StrokeRect {
                rect: self.bounds,
                color: Color::rgba(
                    if self.pressed { 0xd9 } else { 0xc8 },
                    if self.pressed { 0xee } else { 0xe5 },
                    0xff,
                    if self.pressed {
                        (6.0 + pulse * 228.0).round().clamp(0.0, 255.0) as u8
                    } else {
                        (4.0 + pulse * 216.0).round().clamp(0.0, 255.0) as u8
                    },
                ),
            });
        }
        self.paint_trace(scene);
        scene.push(PaintOp::Text {
            rect: Rect {
                x: self.bounds.x + text_inset_x,
                y: self.bounds.y,
                width: (self.bounds.width - text_inset_x * 2.0).max(1.0),
                height: self.bounds.height,
            },
            clip_rect: None,
            text: self.text.clone(),
            style: TextStyle {
                color: text_color,
                font_size: self.font_size,
                horizontal_align: self.text_align,
                vertical_align: VerticalAlign::Middle,
                layout_mode: TextLayoutMode::SingleLine,
                ..TextStyle::default()
            },
        });
    }

    fn paint_trace(&self, scene: &mut Vec<PaintOp>) {
        if !self.hover && !self.pressed {
            return;
        }
        let t = animation_time_seconds();
        let inset = 5.0;
        let accent_x = 16.0_f32.min((self.bounds.width * 0.2).max(9.0));
        let accent_y = 11.0_f32.min((self.bounds.height * 0.36).max(7.0));
        let trace_thickness = if self.pressed { 2 } else { 1 };
        let center_x = self.bounds.x + self.bounds.width * 0.5;
        let tl = Point {
            x: self.bounds.x - 1.0,
            y: self.bounds.y - 1.0,
        };
        let tr = Point {
            x: self.bounds.right() + 1.0,
            y: self.bounds.y - 1.0,
        };
        let bl = Point {
            x: self.bounds.x - 1.0,
            y: self.bounds.bottom() + 1.0,
        };
        let br = Point {
            x: self.bounds.right() + 1.0,
            y: self.bounds.bottom() + 1.0,
        };

        for (phase, points) in [
            (
                0.15_f32,
                vec![
                    Point {
                        x: tl.x + inset,
                        y: tl.y + accent_y,
                    },
                    Point {
                        x: tl.x + inset,
                        y: tl.y + inset,
                    },
                    Point {
                        x: tl.x + accent_x,
                        y: tl.y + inset,
                    },
                ],
            ),
            (
                1.35_f32,
                vec![
                    Point {
                        x: tr.x - accent_x,
                        y: tr.y + inset,
                    },
                    Point {
                        x: tr.x - inset,
                        y: tr.y + inset,
                    },
                    Point {
                        x: tr.x - inset,
                        y: tr.y + accent_y,
                    },
                ],
            ),
            (
                2.55_f32,
                vec![
                    Point {
                        x: bl.x + inset,
                        y: bl.y - accent_y,
                    },
                    Point {
                        x: bl.x + inset,
                        y: bl.y - inset,
                    },
                    Point {
                        x: bl.x + accent_x * 0.82,
                        y: bl.y - inset,
                    },
                ],
            ),
            (
                3.95_f32,
                vec![
                    Point {
                        x: br.x - accent_x * 0.9,
                        y: br.y - inset,
                    },
                    Point {
                        x: br.x - inset,
                        y: br.y - inset,
                    },
                    Point {
                        x: br.x - inset,
                        y: br.y - accent_y,
                    },
                ],
            ),
            (
                4.8_f32,
                vec![
                    Point {
                        x: center_x - accent_x * 0.35,
                        y: self.bounds.y - 2.0,
                    },
                    Point {
                        x: center_x + accent_x * 0.35,
                        y: self.bounds.y - 2.0,
                    },
                ],
            ),
        ] {
            let local_pulse = hover_pulse(t + phase * 0.22, self.pressed);
            scene.push(PaintOp::Polyline {
                points: points.clone(),
                color: self.trace_color(local_pulse),
                thickness: trace_thickness,
                closed: false,
            });
            scene.push(PaintOp::Polyline {
                points,
                color: self.trace_color((local_pulse * 0.72).clamp(0.0, 1.0)),
                thickness: 1,
                closed: false,
            });
        }

        let mut particles = Vec::new();
        for (phase, anchor, offset_x, offset_y) in [
            (
                0.0_f32,
                Point {
                    x: tl.x + inset + 2.0,
                    y: tl.y + inset + 1.0,
                },
                -2.2_f32,
                -1.6_f32,
            ),
            (
                1.2_f32,
                Point {
                    x: tr.x - inset - 2.0,
                    y: tr.y + inset + 2.0,
                },
                2.0_f32,
                -1.4_f32,
            ),
            (
                2.5_f32,
                Point {
                    x: br.x - inset - 3.0,
                    y: br.y - inset - 2.0,
                },
                2.0_f32,
                1.6_f32,
            ),
            (
                4.1_f32,
                Point {
                    x: center_x + accent_x * 0.1,
                    y: self.bounds.y - 3.0,
                },
                0.0_f32,
                -1.8_f32,
            ),
        ] {
            let local_pulse = hover_pulse(t + phase * 0.3, self.pressed);
            particles.push(Particle {
                center: Point {
                    x: anchor.x + offset_x,
                    y: anchor.y + offset_y,
                },
                radius: if local_pulse > 0.72 {
                    if self.pressed {
                        2.3
                    } else {
                        1.9
                    }
                } else {
                    1.2
                },
                color: self.trace_color((local_pulse * 0.9).clamp(0.0, 1.0)),
            });
            if local_pulse > 0.42 {
                particles.push(Particle {
                    center: Point {
                        x: anchor.x + offset_x * 1.8,
                        y: anchor.y + offset_y * 1.8,
                    },
                    radius: 1.0,
                    color: self.trace_color((local_pulse * 0.58).clamp(0.0, 1.0)),
                });
            }
        }
        if particles.is_empty() {
            particles.push(Particle {
                center: Point {
                    x: tr.x - inset - 1.5,
                    y: tr.y + inset + 1.0,
                },
                radius: if self.pressed { 1.8 } else { 1.4 },
                color: self.trace_color(0.0),
            });
        }
        if !particles.is_empty() {
            scene.push(PaintOp::ParticleBatch { particles });
        }
    }

    fn trace_color(&self, alpha_scale: f32) -> Color {
        let base = if self.pressed {
            Color::rgba(0xa8, 0xd1, 0xff, 0xff)
        } else if self.hover {
            Color::rgba(0x98, 0xc6, 0xff, 0xff)
        } else {
            Color::rgba(0x89, 0xb8, 0xf5, 0x74)
        };
        Color::rgba(
            base.r,
            base.g,
            base.b,
            ((base.a as f32) * alpha_scale).round().clamp(0.0, 255.0) as u8,
        )
    }

    fn restart_pulse_cycle(&mut self) {
        self.pulse_origin_seconds = Some(animation_time_seconds_f64());
    }

    fn clear_pulse_cycle_if_inactive(&mut self) {
        if !self.hover && !self.pressed && !self.focused {
            self.pulse_origin_seconds = None;
        }
    }
}

fn hover_pulse(t: f32, pressed: bool) -> f32 {
    let speed = if pressed { 1.05 } else { 0.62 };
    ((t * speed).sin() * 0.5 + 0.5).powf(1.2)
}

const ANIMATION_TIME_WRAP_SECONDS: f64 = 4096.0;

fn animation_time_seconds() -> f32 {
    animation_time_seconds_f64() as f32
}

fn animation_time_seconds_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| {
            duration
                .as_secs_f64()
                .rem_euclid(ANIMATION_TIME_WRAP_SECONDS)
        })
        .unwrap_or(0.0)
}

fn animation_elapsed_seconds(origin: f64) -> f32 {
    let now = animation_time_seconds_f64();
    if now >= origin {
        (now - origin) as f32
    } else {
        (ANIMATION_TIME_WRAP_SECONDS - origin + now) as f32
    }
}

fn lerp_color(from: Color, to: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let lerp = |a: u8, b: u8| -> u8 {
        ((a as f32) + ((b as f32) - (a as f32)) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::rgba(
        lerp(from.r, to.r),
        lerp(from.g, to.g),
        lerp(from.b, to.b),
        lerp(from.a, to.a),
    )
}

#[derive(Debug, Clone, PartialEq)]
pub struct Button {
    pub id: i32,
    pub model: ButtonModel,
}

impl Button {
    pub fn new(id: i32, text: impl Into<String>, bounds: Rect) -> Self {
        Self {
            id,
            model: ButtonModel::with_id(WidgetId(id as u64), text, bounds),
        }
    }

    pub fn handle_event(&mut self, event: UiEvent) -> WidgetResponse {
        self.model.handle_event(event)
    }

    pub fn paint(&self, scene: &mut Vec<PaintOp>) {
        self.model.paint(scene);
    }
}

impl Component for Button {
    fn bounds(&self) -> Rect {
        self.model.bounds
    }

    fn set_bounds(&mut self, rect: Rect) {
        self.model.set_bounds(rect);
    }

    fn focus_changed(&mut self, gained: bool) {
        let _ = self.model.handle_event(UiEvent::FocusChanged(gained));
    }

    fn mouse_entered(&mut self) {
        let position = self.model.bounds;
        let _ = self
            .model
            .handle_event(UiEvent::PointerMoved(crate::input::PointerState::mouse(
                crate::geometry::Point {
                    x: position.x,
                    y: position.y,
                },
                Default::default(),
            )));
    }

    fn mouse_exited(&mut self) {
        let _ = self.model.handle_event(UiEvent::PointerLeft);
    }

    fn id(&self) -> i32 {
        self.id
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        geometry::{Point, Rect},
        input::{Modifiers, PointerButton, PointerState, UiEvent},
        paint::{HorizontalAlign, PaintOp, TextLayoutMode, VerticalAlign},
    };

    use super::ButtonModel;
    use crate::widget::{WidgetAction, WidgetId};

    fn pointer(x: f32, y: f32) -> PointerState {
        PointerState::mouse(Point { x, y }, Modifiers::default())
    }

    #[test]
    fn button_click_emits_activation() {
        let mut button = ButtonModel::with_id(
            WidgetId(7),
            "Run",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 80.0,
                height: 24.0,
            },
        );

        assert!(
            button
                .handle_event(UiEvent::PointerPressed {
                    button: PointerButton::Primary,
                    state: pointer(5.0, 5.0),
                })
                .input_consumed
        );

        let response = button.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: pointer(5.0, 5.0),
        });

        assert_eq!(response.action, Some(WidgetAction::Activate(WidgetId(7))));
        assert!(response.input_consumed);
    }

    #[test]
    fn paint_emits_text_and_background() {
        let button = ButtonModel::new(
            "Run",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 80.0,
                height: 24.0,
            },
        );
        let mut scene = Vec::new();

        button.paint(&mut scene);

        assert!(scene
            .iter()
            .any(|op| matches!(op, PaintOp::FillRect { .. })));
        assert!(scene
            .iter()
            .any(|op| matches!(op, PaintOp::StrokeRect { .. })));
        assert!(scene.iter().any(|op| matches!(op, PaintOp::Text { .. })));
    }

    #[test]
    fn paint_centers_button_text_with_shared_text_contract() {
        let button = ButtonModel::new(
            "Menu",
            Rect {
                x: 10.0,
                y: 20.0,
                width: 108.0,
                height: 44.0,
            },
        );
        let mut scene = Vec::new();

        button.paint(&mut scene);

        let text = scene
            .into_iter()
            .find_map(|op| match op {
                PaintOp::Text {
                    rect, text, style, ..
                } => Some((rect, text, style)),
                _ => None,
            })
            .expect("button paint should emit text");
        assert_eq!(text.0, button.bounds);
        assert_eq!(text.1, "Menu");
        assert_eq!(text.2.horizontal_align, HorizontalAlign::Center);
        assert_eq!(text.2.vertical_align, VerticalAlign::Middle);
        assert_eq!(text.2.layout_mode, TextLayoutMode::SingleLine);
    }

    #[test]
    fn paint_emits_trace_segments_around_button_bounds() {
        let mut button = ButtonModel::new(
            "Trace",
            Rect {
                x: 20.0,
                y: 30.0,
                width: 100.0,
                height: 32.0,
            },
        );
        button.hover = true;
        let mut scene = Vec::new();

        button.paint(&mut scene);

        let trace_segments: Vec<_> = scene
            .iter()
            .filter_map(|op| match op {
                PaintOp::Polyline { points, .. } => Some(points.clone()),
                _ => None,
            })
            .collect();

        assert!(!trace_segments.is_empty());
        assert!(trace_segments.iter().flatten().any(|point| {
            point.x < button.bounds.x
                || point.y < button.bounds.y
                || point.x > button.bounds.right()
                || point.y > button.bounds.bottom()
        }));
        assert!(scene
            .iter()
            .any(|op| matches!(op, PaintOp::ParticleBatch { .. })));
    }

    #[test]
    fn button_release_outside_consumes_press_without_activation() {
        let mut button = ButtonModel::with_id(
            WidgetId(9),
            "Run",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 80.0,
                height: 24.0,
            },
        );
        let press = button.handle_event(UiEvent::PointerPressed {
            button: PointerButton::Primary,
            state: pointer(5.0, 5.0),
        });
        assert!(press.input_consumed);

        let release = button.handle_event(UiEvent::PointerReleased {
            button: PointerButton::Primary,
            state: pointer(200.0, 200.0),
        });
        assert!(release.request_redraw);
        assert!(release.input_consumed);
        assert_eq!(release.action, None);
    }

    #[test]
    fn hover_entry_starts_a_local_pulse_cycle() {
        let mut button = ButtonModel::with_id(
            WidgetId(11),
            "Pulse",
            Rect {
                x: 0.0,
                y: 0.0,
                width: 80.0,
                height: 24.0,
            },
        );

        assert_eq!(button.pulse_origin_seconds, None);
        let _ = button.handle_event(UiEvent::PointerMoved(pointer(5.0, 5.0)));
        assert!(button.pulse_origin_seconds.is_some());
        let _ = button.handle_event(UiEvent::PointerLeft);
        assert_eq!(button.pulse_origin_seconds, None);
    }

    #[test]
    fn interactive_visual_outsets_cover_trace_and_particle_bleed() {
        let outsets = ButtonModel::interactive_visual_outsets();
        assert_eq!(outsets.left, 2.0);
        assert_eq!(outsets.top, 9.0);
        assert_eq!(outsets.right, 6.0);
        assert_eq!(outsets.bottom, 6.0);
    }
}
