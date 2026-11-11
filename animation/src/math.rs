//! Small 3D maths for animation: vectors, unit quaternions and rigid transforms, in `f32`.
//!
//! Conventions: right-handed, Y up, metres. A quaternion `q` rotates a vector as
//! `q * v * q⁻¹`; `a * b` applies `b` first, then `a`. A character at rest faces +Z with
//! its left side towards +X, as glTF and three.js humanoids do.

use std::ops::{Add, AddAssign, Div, Mul, MulAssign, Neg, Sub, SubAssign};

/// A 3-vector.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0);
    pub const X: Self = Self::new(1.0, 0.0, 0.0);
    pub const Y: Self = Self::new(0.0, 1.0, 0.0);
    pub const Z: Self = Self::new(0.0, 0.0, 1.0);

    #[must_use]
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    #[must_use]
    pub fn dot(self, o: Self) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    #[must_use]
    pub fn cross(self, o: Self) -> Self {
        Self::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }

    #[must_use]
    pub fn length_squared(self) -> f32 {
        self.dot(self)
    }

    #[must_use]
    pub fn length(self) -> f32 {
        self.length_squared().sqrt()
    }

    /// The unit vector along `self`, or zero for a (near) zero vector.
    #[must_use]
    pub fn normalize_or_zero(self) -> Self {
        let l = self.length();
        if l > 1e-12 {
            self / l
        } else {
            Self::ZERO
        }
    }

    /// The vector on the ground plane (y = 0).
    #[must_use]
    pub fn flat(self) -> Self {
        Self::new(self.x, 0.0, self.z)
    }

    #[must_use]
    pub fn lerp(self, o: Self, t: f32) -> Self {
        self + (o - self) * t
    }

    #[must_use]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }

    #[must_use]
    pub fn distance(self, o: Self) -> f32 {
        (self - o).length()
    }
}

impl Add for Vec3 {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}
impl AddAssign for Vec3 {
    fn add_assign(&mut self, o: Self) {
        *self = *self + o;
    }
}
impl Sub for Vec3 {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}
impl SubAssign for Vec3 {
    fn sub_assign(&mut self, o: Self) {
        *self = *self - o;
    }
}
impl Mul<f32> for Vec3 {
    type Output = Self;
    fn mul(self, s: f32) -> Self {
        Self::new(self.x * s, self.y * s, self.z * s)
    }
}
impl MulAssign<f32> for Vec3 {
    fn mul_assign(&mut self, s: f32) {
        *self = *self * s;
    }
}
impl Div<f32> for Vec3 {
    type Output = Self;
    fn div(self, s: f32) -> Self {
        Self::new(self.x / s, self.y / s, self.z / s)
    }
}
impl Neg for Vec3 {
    type Output = Self;
    fn neg(self) -> Self {
        Self::new(-self.x, -self.y, -self.z)
    }
}

/// A rotation as a unit quaternion `(x, y, z, w)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quat {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Default for Quat {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Quat {
    pub const IDENTITY: Self = Self::new(0.0, 0.0, 0.0, 1.0);

    #[must_use]
    pub const fn new(x: f32, y: f32, z: f32, w: f32) -> Self {
        Self { x, y, z, w }
    }

    /// A rotation of `angle` radians about the unit vector `axis`.
    #[must_use]
    pub fn from_axis_angle(axis: Vec3, angle: f32) -> Self {
        let (s, c) = (angle * 0.5).sin_cos();
        Self::new(axis.x * s, axis.y * s, axis.z * s, c)
    }

    /// A rotation of `yaw` radians about +Y.
    #[must_use]
    pub fn from_yaw(yaw: f32) -> Self {
        Self::from_axis_angle(Vec3::Y, yaw)
    }

    #[must_use]
    pub fn dot(self, o: Self) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z + self.w * o.w
    }

    #[must_use]
    pub fn conjugate(self) -> Self {
        Self::new(-self.x, -self.y, -self.z, self.w)
    }

    /// The inverse of a unit quaternion.
    #[must_use]
    pub fn inverse(self) -> Self {
        self.conjugate()
    }

    #[must_use]
    pub fn normalize(self) -> Self {
        let l = self.dot(self).sqrt();
        if l > 1e-12 {
            Self::new(self.x / l, self.y / l, self.z / l, self.w / l)
        } else {
            Self::IDENTITY
        }
    }

    /// The same rotation with `w >= 0` (the shorter way round).
    #[must_use]
    pub fn abs(self) -> Self {
        if self.w < 0.0 {
            Self::new(-self.x, -self.y, -self.z, -self.w)
        } else {
            self
        }
    }

    /// `self` or its negation, whichever is nearer `other` (same rotation).
    #[must_use]
    pub fn nearest(self, other: Self) -> Self {
        if self.dot(other) < 0.0 {
            Self::new(-self.x, -self.y, -self.z, -self.w)
        } else {
            self
        }
    }

    /// Rotates `v`.
    #[must_use]
    pub fn rotate(self, v: Vec3) -> Vec3 {
        // v + 2w(q × v) + 2 q × (q × v)
        let q = Vec3::new(self.x, self.y, self.z);
        let t = q.cross(v) * 2.0;
        v + t * self.w + q.cross(t)
    }

    /// `self⁻¹ * v`.
    #[must_use]
    pub fn inverse_rotate(self, v: Vec3) -> Vec3 {
        self.conjugate().rotate(v)
    }

    /// The rotation as a vector along its axis whose length is the angle (radians).
    #[must_use]
    pub fn to_scaled_angle_axis(self) -> Vec3 {
        let q = self.abs();
        let v = Vec3::new(q.x, q.y, q.z);
        let s = v.length();
        if s < 1e-8 {
            return v * 2.0;
        }
        let angle = 2.0 * s.atan2(q.w);
        v * (angle / s)
    }

    /// The inverse of [`Quat::to_scaled_angle_axis`].
    #[must_use]
    pub fn from_scaled_angle_axis(v: Vec3) -> Self {
        let angle = v.length();
        if angle < 1e-8 {
            return Self::new(v.x * 0.5, v.y * 0.5, v.z * 0.5, 1.0).normalize();
        }
        Self::from_axis_angle(v / angle, angle)
    }

    /// Spherical interpolation, the shorter way.
    #[must_use]
    pub fn slerp(self, o: Self, t: f32) -> Self {
        let o = o.nearest(self);
        let d = self.dot(o).clamp(-1.0, 1.0);
        if d > 0.9995 {
            return Self::new(
                self.x + (o.x - self.x) * t,
                self.y + (o.y - self.y) * t,
                self.z + (o.z - self.z) * t,
                self.w + (o.w - self.w) * t,
            )
            .normalize();
        }
        let theta = d.acos();
        let s = theta.sin();
        let a = ((1.0 - t) * theta).sin() / s;
        let b = (t * theta).sin() / s;
        Self::new(
            self.x * a + o.x * b,
            self.y * a + o.y * b,
            self.z * a + o.z * b,
            self.w * a + o.w * b,
        )
    }

    /// The shortest rotation taking unit vector `a` to unit vector `b`.
    #[must_use]
    pub fn from_to(a: Vec3, b: Vec3) -> Self {
        let d = a.dot(b);
        if d < -0.999_999 {
            // Opposite: any perpendicular axis.
            let mut axis = Vec3::X.cross(a);
            if axis.length_squared() < 1e-6 {
                axis = Vec3::Y.cross(a);
            }
            return Self::from_axis_angle(axis.normalize_or_zero(), std::f32::consts::PI);
        }
        let c = a.cross(b);
        Self::new(c.x, c.y, c.z, 1.0 + d).normalize()
    }

    /// The rotation whose columns are the orthonormal basis `(x, y, z)`.
    #[must_use]
    pub fn from_basis(x: Vec3, y: Vec3, z: Vec3) -> Self {
        let (m00, m10, m20) = (x.x, x.y, x.z);
        let (m01, m11, m21) = (y.x, y.y, y.z);
        let (m02, m12, m22) = (z.x, z.y, z.z);
        let trace = m00 + m11 + m22;
        let q = if trace > 0.0 {
            let s = (trace + 1.0).sqrt() * 2.0;
            Self::new((m21 - m12) / s, (m02 - m20) / s, (m10 - m01) / s, 0.25 * s)
        } else if m00 > m11 && m00 > m22 {
            let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
            Self::new(0.25 * s, (m01 + m10) / s, (m02 + m20) / s, (m21 - m12) / s)
        } else if m11 > m22 {
            let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
            Self::new((m01 + m10) / s, 0.25 * s, (m12 + m21) / s, (m02 - m20) / s)
        } else {
            let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
            Self::new((m02 + m20) / s, (m12 + m21) / s, 0.25 * s, (m10 - m01) / s)
        };
        q.normalize()
    }

    /// The heading of `self`: the angle about +Y of where it turns +Z.
    #[must_use]
    pub fn yaw(self) -> f32 {
        let f = self.rotate(Vec3::Z);
        f.x.atan2(f.z)
    }

    /// The angle in radians between two rotations (accurate for small angles too).
    #[must_use]
    pub fn angle_to(self, o: Self) -> f32 {
        let d = self.conjugate() * o;
        let v = Vec3::new(d.x, d.y, d.z).length();
        2.0 * v.atan2(d.w.abs())
    }

    #[must_use]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite() && self.w.is_finite()
    }

    /// Mirrored across the YZ plane (x ↦ -x): the rotation a mirror image performs.
    #[must_use]
    pub fn mirror_x(self) -> Self {
        Self::new(self.x, -self.y, -self.z, self.w)
    }

    /// From intrinsic Euler angles about the axes named in `order` (each `b'X'`, `b'Y'`
    /// or `b'Z'`), applied as `R(order[0]) * R(order[1]) * R(order[2])`, which is how BVH
    /// channels compose.
    #[must_use]
    pub fn from_euler_order(order: [u8; 3], angles: [f32; 3]) -> Self {
        let mut q = Self::IDENTITY;
        for (axis, angle) in order.iter().zip(angles) {
            let a = match axis {
                b'X' => Vec3::X,
                b'Y' => Vec3::Y,
                _ => Vec3::Z,
            };
            q = q * Self::from_axis_angle(a, angle);
        }
        q
    }
}

impl Mul for Quat {
    type Output = Self;
    fn mul(self, o: Self) -> Self {
        Self::new(
            self.w * o.x + self.x * o.w + self.y * o.z - self.z * o.y,
            self.w * o.y - self.x * o.z + self.y * o.w + self.z * o.x,
            self.w * o.z + self.x * o.y - self.y * o.x + self.z * o.w,
            self.w * o.w - self.x * o.x - self.y * o.y - self.z * o.z,
        )
    }
}

/// A rigid transform: rotate, then translate.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Transform {
    pub translation: Vec3,
    pub rotation: Quat,
}

impl Transform {
    pub const IDENTITY: Self = Self {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
    };

    #[must_use]
    pub const fn new(translation: Vec3, rotation: Quat) -> Self {
        Self {
            translation,
            rotation,
        }
    }

    /// `self ∘ o`: `o` first, then `self`.
    #[must_use]
    pub fn compose(self, o: Self) -> Self {
        Self::new(
            self.translation + self.rotation.rotate(o.translation),
            self.rotation * o.rotation,
        )
    }

    #[must_use]
    pub fn inverse(self) -> Self {
        let r = self.rotation.inverse();
        Self::new(-r.rotate(self.translation), r)
    }

    #[must_use]
    pub fn apply(self, p: Vec3) -> Vec3 {
        self.translation + self.rotation.rotate(p)
    }

    #[must_use]
    pub fn inverse_apply(self, p: Vec3) -> Vec3 {
        self.rotation.inverse_rotate(p - self.translation)
    }
}

/// Wraps an angle into (-π, π].
#[must_use]
pub fn wrap_angle(a: f32) -> f32 {
    let mut a = a % std::f32::consts::TAU;
    if a > std::f32::consts::PI {
        a -= std::f32::consts::TAU;
    } else if a <= -std::f32::consts::PI {
        a += std::f32::consts::TAU;
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-5
    }

    #[test]
    fn quaternion_rotation_matches_axis_angle() {
        let q = Quat::from_axis_angle(Vec3::Y, std::f32::consts::FRAC_PI_2);
        assert!(close(q.rotate(Vec3::Z), Vec3::X));
        assert!(close(q.inverse_rotate(Vec3::X), Vec3::Z));
        assert!((q.yaw() - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
    }

    #[test]
    fn scaled_angle_axis_round_trips() {
        let q = Quat::from_axis_angle(Vec3::new(1.0, 2.0, 3.0).normalize_or_zero(), 1.234);
        let v = q.to_scaled_angle_axis();
        assert!((v.length() - 1.234).abs() < 1e-5);
        let back = Quat::from_scaled_angle_axis(v);
        assert!(back.angle_to(q) < 1e-4);
    }

    #[test]
    fn from_to_and_basis_agree() {
        let a = Vec3::new(0.3, 0.9, -0.2).normalize_or_zero();
        let b = Vec3::new(-0.5, 0.1, 0.8).normalize_or_zero();
        assert!(close(Quat::from_to(a, b).rotate(a), b));
        let q = Quat::from_axis_angle(Vec3::new(0.2, -0.7, 0.4).normalize_or_zero(), 2.1);
        let r = Quat::from_basis(q.rotate(Vec3::X), q.rotate(Vec3::Y), q.rotate(Vec3::Z));
        assert!(r.angle_to(q) < 1e-4);
    }

    #[test]
    fn transforms_compose_and_invert() {
        let a = Transform::new(Vec3::new(1.0, 2.0, 3.0), Quat::from_yaw(0.7));
        let b = Transform::new(
            Vec3::new(-0.5, 0.0, 2.0),
            Quat::from_axis_angle(Vec3::X, 0.3),
        );
        let p = Vec3::new(0.1, 0.2, 0.3);
        assert!(close(a.compose(b).apply(p), a.apply(b.apply(p))));
        assert!(close(a.inverse().apply(a.apply(p)), p));
    }

    #[test]
    fn euler_order_composes_left_to_right() {
        let q = Quat::from_euler_order(*b"ZXY", [0.1, 0.2, 0.3]);
        let manual = Quat::from_axis_angle(Vec3::Z, 0.1)
            * Quat::from_axis_angle(Vec3::X, 0.2)
            * Quat::from_axis_angle(Vec3::Y, 0.3);
        assert!(q.angle_to(manual) < 1e-6);
    }

    #[test]
    fn mirror_reflects_rotation() {
        let q = Quat::from_axis_angle(Vec3::new(0.3, 0.5, 0.8).normalize_or_zero(), 0.9);
        let v = Vec3::new(0.4, -0.2, 0.7);
        let m = |p: Vec3| Vec3::new(-p.x, p.y, p.z);
        assert!(close(q.mirror_x().rotate(m(v)), m(q.rotate(v))));
    }
}
