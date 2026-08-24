//! Generic virtual joystick tracking: turns a tracked touch id and a moving
//! point into a clamped, dead-zoned output vector. Purely geometric — no
//! rendering, no snapping, no game semantics. Callers own touch routing
//! (which touch id belongs to which stick) and any output shaping (angle
//! snapping, button semantics, etc).

fn vec_sub(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    (a.0 - b.0, a.1 - b.1)
}

fn vec_length(v: (f32, f32)) -> f32 {
    (v.0 * v.0 + v.1 * v.1).sqrt()
}

fn vec_clamp_length(v: (f32, f32), max_length: f32) -> (f32, f32) {
    let length = vec_length(v);
    if length <= max_length || length <= f32::EPSILON {
        return v;
    }
    let scale = max_length / length;
    (v.0 * scale, v.1 * scale)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VirtualJoystick {
    origin: Option<(f32, f32)>,
    current: Option<(f32, f32)>,
    tracked_touch_id: Option<u64>,
    radius: f32,
    deadzone: f32,
}

impl VirtualJoystick {
    /// `radius` is the maximum thumb travel distance (logical units) before
    /// the output clamps to full magnitude. `deadzone` is a fraction of
    /// `radius` (0.0..=1.0) within which the output stays zero.
    pub fn new(radius: f32, deadzone: f32) -> Self {
        Self {
            origin: None,
            current: None,
            tracked_touch_id: None,
            radius: radius.max(f32::EPSILON),
            deadzone: deadzone.clamp(0.0, 1.0),
        }
    }

    /// Whether `point` falls within `base_radius` of `base_center` — the
    /// touch-down hit test callers use to decide which stick (if any) a new
    /// touch should start tracking.
    pub fn bounds_contains(
        &self,
        base_center: (f32, f32),
        base_radius: f32,
        point: (f32, f32),
    ) -> bool {
        vec_length(vec_sub(point, base_center)) <= base_radius
    }

    /// Begin tracking `touch_id` at `at`. Replaces any previously tracked
    /// touch (callers are expected to have already routed touch-down
    /// events so this is only called for touches meant for this stick).
    pub fn begin(&mut self, touch_id: u64, at: (f32, f32)) {
        self.origin = Some(at);
        self.current = Some(at);
        self.tracked_touch_id = Some(touch_id);
    }

    /// Update the tracked touch's position. A no-op if `touch_id` is not
    /// the currently tracked touch.
    pub fn update(&mut self, touch_id: u64, at: (f32, f32)) {
        if self.tracked_touch_id == Some(touch_id) {
            self.current = Some(at);
        }
    }

    /// Stop tracking `touch_id` and reset to the neutral state. A no-op if
    /// `touch_id` is not the currently tracked touch.
    pub fn end(&mut self, touch_id: u64) {
        if self.tracked_touch_id == Some(touch_id) {
            self.origin = None;
            self.current = None;
            self.tracked_touch_id = None;
        }
    }

    pub fn is_active(&self) -> bool {
        self.tracked_touch_id.is_some()
    }

    fn clamped_delta(&self) -> (f32, f32) {
        let (Some(origin), Some(current)) = (self.origin, self.current) else {
            return (0.0, 0.0);
        };
        vec_clamp_length(vec_sub(current, origin), self.radius)
    }

    /// Normalized output vector; magnitude is 0 inside the deadzone and
    /// scales linearly up to 1 at `radius` thumb travel.
    pub fn output(&self) -> (f32, f32) {
        let delta = self.clamped_delta();
        let length = vec_length(delta);
        let deadzone_length = self.deadzone * self.radius;
        if length <= deadzone_length {
            return (0.0, 0.0);
        }
        (delta.0 / self.radius, delta.1 / self.radius)
    }

    /// Where to draw the thumb: `base_center` offset by the clamped
    /// (unnormalized) drag delta.
    pub fn thumb_render_position(&self, base_center: (f32, f32)) -> (f32, f32) {
        let delta = self.clamped_delta();
        (base_center.0 + delta.0, base_center.1 + delta.1)
    }
}

#[cfg(test)]
mod tests {
    use super::VirtualJoystick;

    #[test]
    fn deadzone_suppresses_tiny_drags() {
        let mut stick = VirtualJoystick::new(100.0, 0.2);
        stick.begin(1, (0.0, 0.0));
        stick.update(1, (10.0, 0.0));
        assert_eq!(stick.output(), (0.0, 0.0));
    }

    #[test]
    fn output_clamps_to_unit_magnitude_beyond_radius() {
        let mut stick = VirtualJoystick::new(100.0, 0.0);
        stick.begin(1, (0.0, 0.0));
        stick.update(1, (500.0, 0.0));
        let (x, y) = stick.output();
        assert!((x - 1.0).abs() < 1e-4);
        assert!(y.abs() < 1e-4);
    }

    #[test]
    fn output_scales_linearly_within_radius() {
        let mut stick = VirtualJoystick::new(100.0, 0.0);
        stick.begin(1, (0.0, 0.0));
        stick.update(1, (50.0, 0.0));
        let (x, y) = stick.output();
        assert!((x - 0.5).abs() < 1e-4);
        assert!(y.abs() < 1e-4);
    }

    #[test]
    fn mismatched_touch_id_is_ignored() {
        let mut stick = VirtualJoystick::new(100.0, 0.0);
        stick.begin(1, (0.0, 0.0));
        stick.update(2, (500.0, 500.0));
        assert_eq!(stick.output(), (0.0, 0.0));
        assert!(stick.is_active());

        stick.end(2);
        assert!(stick.is_active());
    }

    #[test]
    fn end_zeroes_output_and_deactivates() {
        let mut stick = VirtualJoystick::new(100.0, 0.0);
        stick.begin(1, (0.0, 0.0));
        stick.update(1, (500.0, 0.0));
        stick.end(1);
        assert_eq!(stick.output(), (0.0, 0.0));
        assert!(!stick.is_active());
    }

    #[test]
    fn bounds_contains_uses_circular_hit_test() {
        let stick = VirtualJoystick::new(100.0, 0.0);
        assert!(stick.bounds_contains((200.0, 200.0), 80.0, (250.0, 200.0)));
        assert!(!stick.bounds_contains((200.0, 200.0), 80.0, (300.0, 200.0)));
    }

    #[test]
    fn thumb_render_position_matches_clamped_delta() {
        let mut stick = VirtualJoystick::new(50.0, 0.0);
        stick.begin(1, (10.0, 10.0));
        stick.update(1, (10.0, 500.0));
        let (x, y) = stick.thumb_render_position((10.0, 10.0));
        assert!((x - 10.0).abs() < 1e-4);
        assert!((y - 60.0).abs() < 1e-4);
    }
}
