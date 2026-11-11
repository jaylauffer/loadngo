//! Critically damped springs and inertialization.
//!
//! The springs are exact (closed form over any `dt`), after Daniel Holden's "Spring-It-On"
//! (2021). Inertialization records the difference between where a bone was and where the
//! new animation starts (position or rotation, and velocity) and decays that offset to
//! zero like a critically damped spring released at rest, so a transition keeps the pose
//! and its velocity continuous and then follows the new animation.

use crate::math::{Quat, Vec3};

/// Damping for a spring that halves its error every `halflife` seconds.
#[must_use]
pub fn halflife_to_damping(halflife: f32) -> f32 {
    (4.0 * std::f32::consts::LN_2) / (halflife + 1e-5)
}

/// Decays an offset `x` with velocity `v` towards zero over `dt`.
pub fn decay(x: &mut Vec3, v: &mut Vec3, halflife: f32, dt: f32) {
    let y = halflife_to_damping(halflife) / 2.0;
    let j1 = *v + *x * y;
    let eydt = (-y * dt).exp();
    *x = (*x + j1 * dt) * eydt;
    *v = (*v - j1 * (y * dt)) * eydt;
}

/// Decays a rotation offset `q` with angular velocity `w` towards identity over `dt`.
pub fn decay_rotation(q: &mut Quat, w: &mut Vec3, halflife: f32, dt: f32) {
    let mut x = q.to_scaled_angle_axis();
    decay(&mut x, w, halflife, dt);
    *q = Quat::from_scaled_angle_axis(x);
}

/// An inertialized position: the animation's value plus a decaying offset.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct InertialVec3 {
    pub offset: Vec3,
    pub offset_vel: Vec3,
}

impl InertialVec3 {
    /// A transition from `(src, src_v)` (what was shown) to `(dst, dst_v)` (the new
    /// animation): the shown value and velocity stay continuous.
    pub fn transition(&mut self, src: Vec3, src_v: Vec3, dst: Vec3, dst_v: Vec3) {
        self.offset = (src + self.offset) - dst;
        self.offset_vel = (src_v + self.offset_vel) - dst_v;
    }

    /// Advances by `dt` and returns the shown value for animation value `x`.
    pub fn update(&mut self, x: Vec3, halflife: f32, dt: f32) -> Vec3 {
        decay(&mut self.offset, &mut self.offset_vel, halflife, dt);
        x + self.offset
    }

    /// The shown value without advancing.
    #[must_use]
    pub fn apply(&self, x: Vec3) -> Vec3 {
        x + self.offset
    }
}

/// An inertialized rotation.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct InertialQuat {
    pub offset: Quat,
    pub offset_vel: Vec3,
}

impl InertialQuat {
    /// As [`InertialVec3::transition`], for rotations and angular velocities (rad/s).
    pub fn transition(&mut self, src: Quat, src_w: Vec3, dst: Quat, dst_w: Vec3) {
        self.offset = (self.offset * src * dst.inverse()).abs().normalize();
        self.offset_vel = (src_w + self.offset_vel) - dst_w;
    }

    /// Advances by `dt` and returns the shown rotation for animation rotation `q`.
    pub fn update(&mut self, q: Quat, halflife: f32, dt: f32) -> Quat {
        decay_rotation(&mut self.offset, &mut self.offset_vel, halflife, dt);
        (self.offset * q).normalize()
    }

    /// The shown rotation without advancing.
    #[must_use]
    pub fn apply(&self, q: Quat) -> Quat {
        (self.offset * q).normalize()
    }
}

/// The angular velocity (rad/s, world-ish frame of the rotations) taking `a` to `b` in `dt`.
#[must_use]
pub fn angular_velocity(a: Quat, b: Quat, dt: f32) -> Vec3 {
    (b * a.inverse()).abs().to_scaled_angle_axis() / dt
}

/// A position driven towards a desired velocity by a critically damped spring on its
/// velocity: the character's simulated trajectory.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct VelocitySpring {
    pub position: Vec3,
    pub velocity: Vec3,
    pub acceleration: Vec3,
}

impl VelocitySpring {
    /// Advances by `dt` towards `desired` velocity.
    pub fn update(&mut self, desired: Vec3, halflife: f32, dt: f32) {
        let y = halflife_to_damping(halflife) / 2.0;
        let j0 = self.velocity - desired;
        let j1 = self.acceleration + j0 * y;
        let eydt = (-y * dt).exp();
        let p = self.position;
        self.position = (((-j1) / (y * y)) + ((-j0 - j1 * dt) / y)) * eydt
            + j1 / (y * y)
            + j0 / y
            + desired * dt
            + p;
        self.velocity = (j0 + j1 * dt) * eydt + desired;
        self.acceleration = (self.acceleration - j1 * (y * dt)) * eydt;
    }
}

/// A rotation driven towards a desired rotation by a critically damped spring.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RotationSpring {
    pub rotation: Quat,
    pub angular_velocity: Vec3,
}

impl RotationSpring {
    /// Advances by `dt` towards `desired`.
    pub fn update(&mut self, desired: Quat, halflife: f32, dt: f32) {
        let y = halflife_to_damping(halflife) / 2.0;
        let j0 = (self.rotation * desired.inverse())
            .abs()
            .to_scaled_angle_axis();
        let j1 = self.angular_velocity + j0 * y;
        let eydt = (-y * dt).exp();
        self.rotation = (Quat::from_scaled_angle_axis((j0 + j1 * dt) * eydt) * desired).normalize();
        self.angular_velocity = (self.angular_velocity - j1 * (y * dt)) * eydt;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decay_halves_in_a_halflife_from_rest_and_is_step_independent() {
        let mut a = Vec3::new(1.0, 0.0, 0.0);
        let mut av = Vec3::ZERO;
        decay(&mut a, &mut av, 0.2, 0.2);
        // A critically damped release from rest is not a pure exponential, but close.
        assert!(a.x > 0.5 && a.x < 0.9, "{a:?}");
        let mut b = Vec3::new(1.0, 0.0, 0.0);
        let mut bv = Vec3::ZERO;
        for _ in 0..20 {
            decay(&mut b, &mut bv, 0.2, 0.01);
        }
        assert!((a - b).length() < 1e-4);
    }

    #[test]
    fn inertialization_keeps_the_shown_value_continuous() {
        let mut i = InertialVec3::default();
        let src = Vec3::new(1.0, 2.0, 3.0);
        let dst = Vec3::new(1.5, 2.0, 2.0);
        i.transition(src, Vec3::ZERO, dst, Vec3::ZERO);
        assert!((i.apply(dst) - src).length() < 1e-6);
        for _ in 0..200 {
            i.update(dst, 0.1, 1.0 / 60.0);
        }
        assert!((i.apply(dst) - dst).length() < 1e-3);
        let mut q = InertialQuat::default();
        let qs = Quat::from_yaw(0.4);
        let qd = Quat::from_yaw(-0.2);
        q.offset = Quat::IDENTITY;
        q.transition(qs, Vec3::ZERO, qd, Vec3::ZERO);
        assert!(q.apply(qd).angle_to(qs) < 1e-5);
    }

    #[test]
    fn velocity_spring_reaches_its_target_velocity() {
        let mut s = VelocitySpring::default();
        for _ in 0..120 {
            s.update(Vec3::new(0.0, 0.0, 1.2), 0.2, 1.0 / 60.0);
        }
        assert!((s.velocity.z - 1.2).abs() < 1e-3);
        assert!(s.position.z > 1.5 && s.position.z < 2.4);
        // One big step equals many small ones.
        let mut a = VelocitySpring::default();
        a.update(Vec3::new(1.0, 0.0, 0.0), 0.3, 0.5);
        let mut b = VelocitySpring::default();
        for _ in 0..50 {
            b.update(Vec3::new(1.0, 0.0, 0.0), 0.3, 0.01);
        }
        assert!((a.position - b.position).length() < 1e-4);
    }

    #[test]
    fn rotation_spring_turns_to_its_target() {
        let mut s = RotationSpring {
            rotation: Quat::IDENTITY,
            angular_velocity: Vec3::ZERO,
        };
        for _ in 0..120 {
            s.update(Quat::from_yaw(1.5), 0.2, 1.0 / 60.0);
        }
        assert!(s.rotation.angle_to(Quat::from_yaw(1.5)) < 1e-3);
    }
}
