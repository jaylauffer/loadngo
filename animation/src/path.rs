//! Where the character wants to go: steering for the simulated trajectory.
//!
//! The runtime evaluates a [`Steering`] at the simulated position now and at the positions
//! it predicts over the next second, so a path's coming turn shapes the predicted
//! trajectory before the character reaches it.

use crate::math::Vec3;

/// The desired ground velocity (m/s, y ignored) and facing (radians about +Y, 0 = +Z) for
/// a character whose simulated position is `position`.
pub trait Steering {
    fn desired(&self, position: Vec3) -> (Vec3, f32);

    /// Where the steering means the character to stop, if anywhere (a path's end):
    /// `Config::arrive_exact` brings the character onto it.
    fn goal(&self) -> Option<Vec3> {
        None
    }
}

/// The same velocity and facing everywhere: a stick, a key, or a host that steers itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConstantSteering {
    pub velocity: Vec3,
    pub facing: f32,
}

impl Steering for ConstantSteering {
    fn desired(&self, _position: Vec3) -> (Vec3, f32) {
        (self.velocity, self.facing)
    }
}

/// Up to this many points in a [`PathSteering`].
pub const MAX_PATH_POINTS: usize = 64;

/// Follows a polyline on the ground at a cruising speed, braking to stop at its end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathSteering {
    points: [Vec3; MAX_PATH_POINTS],
    count: usize,
    /// Cruising speed (m/s).
    pub speed: f32,
    /// How far ahead along the path the character steers towards (m).
    pub look_ahead: f32,
    /// Deceleration used to brake to a stop at the end (m/s²).
    pub braking: f32,
    /// Facing once stopped (radians); `None` keeps the last segment's direction.
    pub end_facing: Option<f32>,
    /// Within this distance of the end (m), face `end_facing` already while still walking:
    /// the turn goes with the last steps rather than on the spot (a pivot on a planted foot
    /// slides its sole). 0: only once stopped.
    pub face_from: f32,
    /// Within this distance of the end (m), or past it, the path is done: stand still.
    /// A person a few centimetres off their mark stands; they do not shuffle onto it.
    pub arrive_radius: f32,
}

impl PathSteering {
    /// A path through `points` (at most [`MAX_PATH_POINTS`]; extra points are dropped).
    #[must_use]
    pub fn new(points: &[Vec3], speed: f32) -> Self {
        let mut p = [Vec3::ZERO; MAX_PATH_POINTS];
        let count = points.len().min(MAX_PATH_POINTS);
        for (d, s) in p.iter_mut().zip(points) {
            *d = s.flat();
        }
        Self {
            points: p,
            count,
            speed,
            look_ahead: 0.6,
            braking: 0.9,
            end_facing: None,
            face_from: 0.0,
            arrive_radius: 0.12,
        }
    }

    /// The path's points.
    #[must_use]
    pub fn points(&self) -> &[Vec3] {
        &self.points[..self.count]
    }

    /// The nearest point on the path to `p`: (segment, fraction along it, distance along
    /// the whole path).
    fn project(&self, p: Vec3) -> (usize, f32, f32) {
        let mut best = (0, 0.0, 0.0);
        let mut best_d = f32::INFINITY;
        let mut along = 0.0;
        for s in 0..self.count.saturating_sub(1) {
            let a = self.points[s];
            let b = self.points[s + 1];
            let ab = b - a;
            let len2 = ab.length_squared();
            let t = if len2 > 1e-12 {
                ((p - a).dot(ab) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let d = (a + ab * t - p.flat()).length_squared();
            if d < best_d {
                best_d = d;
                best = (s, t, along + len2.sqrt() * t);
            }
            along += len2.sqrt();
        }
        best
    }

    /// The total length of the path (m).
    #[must_use]
    pub fn length(&self) -> f32 {
        (0..self.count.saturating_sub(1))
            .map(|s| (self.points[s + 1] - self.points[s]).length())
            .sum()
    }

    /// The point `distance` metres along the path (clamped to its ends).
    #[must_use]
    pub fn point_at(&self, distance: f32) -> Vec3 {
        if self.count == 0 {
            return Vec3::ZERO;
        }
        let mut left = distance.max(0.0);
        for s in 0..self.count.saturating_sub(1) {
            let a = self.points[s];
            let b = self.points[s + 1];
            let len = (b - a).length();
            if left <= len && len > 1e-9 {
                return a + (b - a) * (left / len);
            }
            left -= len;
        }
        self.points[self.count - 1]
    }

    fn last_direction(&self) -> f32 {
        if self.count < 2 {
            return 0.0;
        }
        let d = self.points[self.count - 1] - self.points[self.count - 2];
        d.x.atan2(d.z)
    }
}

impl Steering for PathSteering {
    fn goal(&self) -> Option<Vec3> {
        (self.count >= 2).then(|| self.points[self.count - 1])
    }

    fn desired(&self, position: Vec3) -> (Vec3, f32) {
        if self.count < 2 {
            return (Vec3::ZERO, self.end_facing.unwrap_or(0.0));
        }
        let total = self.length();
        let (_, _, along) = self.project(position);
        let end = self.points[self.count - 1];
        let to_end = (end - position.flat()).length();
        // Past the path's end the projection clamps there; then it is the straight distance.
        let remaining = if total - along > 1e-3 {
            total - along
        } else {
            to_end
        };
        let stop_facing = self.end_facing.unwrap_or_else(|| self.last_direction());
        // Arrived: near the end, or past it along the last segment (an overshoot is not a
        // reason to turn back).
        let last = (end - self.points[self.count - 2]).normalize_or_zero();
        let past = (position.flat() - end).dot(last) > 0.0;
        if to_end < self.arrive_radius || (past && to_end < 0.75) {
            return (Vec3::ZERO, stop_facing);
        }
        let target = self.point_at(along + self.look_ahead);
        let dir = (target - position.flat()).normalize_or_zero();
        // Brake so as to stop where the path is done (the arrive radius short of its end): v² = 2 a
        // d, never faster than cruising. (Braking to the end itself, the speed wanted was still
        // 0.43 m/s at the arrive radius and fell to nothing there in a frame.)
        let speed = self
            .speed
            .min((2.0 * self.braking * (remaining - self.arrive_radius).max(0.0)).sqrt());
        let facing = if speed > 0.05 && !(self.end_facing.is_some() && remaining < self.face_from) {
            dir.x.atan2(dir.z)
        } else {
            stop_facing
        };
        (dir * speed, facing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steers_along_and_stops_at_the_end() {
        let p = PathSteering::new(
            &[
                Vec3::ZERO,
                Vec3::new(0.0, 0.0, 5.0),
                Vec3::new(5.0, 0.0, 5.0),
            ],
            1.0,
        );
        assert!((p.length() - 10.0).abs() < 1e-5);
        let (v, f) = p.desired(Vec3::new(0.0, 0.0, 1.0));
        assert!((v.length() - 1.0).abs() < 1e-4 && v.z > 0.9);
        assert!(f.abs() < 0.1);
        // Near the corner it aims round it.
        let (v, _) = p.desired(Vec3::new(0.0, 0.0, 4.8));
        assert!(v.x > 0.1, "{v:?}");
        // Near the end it brakes.
        let (v, f) = p.desired(Vec3::new(4.9, 0.0, 5.0));
        assert!(v.length() < 0.5);
        assert!((f - std::f32::consts::FRAC_PI_2).abs() < 0.2);
        let (v, _) = p.desired(Vec3::new(5.0, 0.0, 5.0));
        assert!(v.length() < 1e-6);
        // A little past the end: still done, no turning back.
        let (v, f) = p.desired(Vec3::new(5.4, 0.0, 5.0));
        assert!(v.length() < 1e-6);
        assert!((f - std::f32::consts::FRAC_PI_2).abs() < 0.01);
    }
}
