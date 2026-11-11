//! Two-bone inverse kinematics (hip, knee, ankle), with a soft reach.

use crate::math::{Quat, Vec3};

/// A two-bone chain's solution: new world rotations of its root (hip) and middle (knee).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TwoBone {
    pub root: Quat,
    pub mid: Quat,
}

/// The distance a chain of full length `full` reaches towards a target `want` away, with
/// the last `soft` fraction of its length approached ever more slowly, so a leg nearing
/// full stretch never snaps straight.
#[must_use]
pub fn soft_reach(want: f32, full: f32, soft: f32) -> f32 {
    let knee = full * (1.0 - soft);
    if want <= knee || soft <= 0.0 {
        return want.min(full);
    }
    let s = full * soft;
    knee + s * (1.0 - (-(want - knee) / s).exp())
}

/// Solves hip `a`, knee `b`, ankle `c` (world positions) so the ankle reaches `target`,
/// bending in the leg's current plane (or about `pole` × the leg when the leg is straight;
/// `pole` is where the knee should point, such as the body's forward).
/// `root`/`mid` are the hip's and knee's world rotations now.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn two_bone(
    a: Vec3,
    b: Vec3,
    c: Vec3,
    target: Vec3,
    pole: Vec3,
    soft: f32,
    root: Quat,
    mid: Quat,
) -> TwoBone {
    let lab = (b - a).length();
    let lcb = (c - b).length();
    if lab < 1e-6 || lcb < 1e-6 {
        return TwoBone { root, mid };
    }
    let to_t = target - a;
    let want = to_t.length();
    let lat = soft_reach(want, lab + lcb, soft).max(1e-5);
    let t = if want > 1e-6 {
        a + to_t * (lat / want)
    } else {
        c
    };

    // The plane the knee bends in: the leg's own while it is clearly bent, turning towards
    // the pole's as it straightens, where the leg's own plane is ill-defined and would
    // swing the knee about from frame to frame.
    let leg = (c - a).normalize_or_zero();
    let own = leg.cross((b - a).normalize_or_zero());
    let by_pole = leg.cross(pole).normalize_or_zero();
    let bend = own.length(); // sine of the angle at the hip between thigh and hip-to-ankle
    let w = ((bend - 0.02) / 0.13).clamp(0.0, 1.0);
    let w = w * w * (3.0 - 2.0 * w);
    let axis = (own.normalize_or_zero() * w + by_pole * (1.0 - w)).normalize_or_zero();
    if axis.length_squared() < 0.5 {
        return TwoBone { root, mid };
    }
    let clamp = |x: f32| x.clamp(-1.0, 1.0);
    let ac_ab_0 = clamp((c - a).normalize_or_zero().dot((b - a).normalize_or_zero())).acos();
    let ba_bc_0 = clamp((a - b).normalize_or_zero().dot((c - b).normalize_or_zero())).acos();
    let ac_ab_1 = clamp((lab * lab + lat * lat - lcb * lcb) / (2.0 * lab * lat)).acos();
    let ba_bc_1 = clamp((lab * lab + lcb * lcb - lat * lat) / (2.0 * lab * lcb)).acos();
    // A positive turn about n = (c - a) × (b - a) opens both angles (it carries the thigh
    // away from the hip-to-ankle line, and the shin away from the thigh), so each turns by
    // its new angle less its old.
    let r0 = Quat::from_axis_angle(axis, ac_ab_1 - ac_ab_0);
    let r1 = Quat::from_axis_angle(axis, ba_bc_1 - ba_bc_0);
    // Swing the whole chain onto the target.
    let c1 = a + r0.rotate(b - a) + (r0 * r1).rotate(c - b);
    let r2 = Quat::from_to((c1 - a).normalize_or_zero(), (t - a).normalize_or_zero());
    TwoBone {
        root: (r2 * r0 * root).normalize(),
        mid: (r2 * r0 * r1 * mid).normalize(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solve_and_end(a: Vec3, b: Vec3, c: Vec3, t: Vec3) -> (Vec3, Vec3) {
        let s = two_bone(a, b, c, t, Vec3::Z, 0.0, Quat::IDENTITY, Quat::IDENTITY);
        let knee = a + s.root.rotate(b - a);
        let end = knee + s.mid.rotate(c - b);
        (knee, end)
    }

    #[test]
    fn reaches_a_target_in_range_and_keeps_bone_lengths() {
        let a = Vec3::new(0.0, 1.0, 0.0);
        let b = Vec3::new(0.0, 0.55, 0.05);
        let c = Vec3::new(0.0, 0.1, 0.0);
        for t in [
            Vec3::new(0.1, 0.3, 0.2),
            Vec3::new(-0.1, 0.4, -0.3),
            Vec3::new(0.0, 0.2, 0.05),
        ] {
            let (knee, end) = solve_and_end(a, b, c, t);
            assert!((end - t).length() < 1e-4, "target {t:?} end {end:?}");
            assert!(((knee - a).length() - (b - a).length()).abs() < 1e-4);
            assert!(((end - knee).length() - (c - b).length()).abs() < 1e-4);
        }
    }

    #[test]
    fn keeps_the_knee_on_its_side() {
        let a = Vec3::new(0.0, 1.0, 0.0);
        let b = Vec3::new(0.0, 0.55, 0.05); // knee forward (+Z)
        let c = Vec3::new(0.0, 0.1, 0.0);
        let (knee, _) = solve_and_end(a, b, c, Vec3::new(0.0, 0.35, 0.0));
        assert!(knee.z > 0.1, "{knee:?}");
    }

    #[test]
    fn soft_reach_never_exceeds_the_chain() {
        assert!((soft_reach(0.5, 1.0, 0.04) - 0.5).abs() < 1e-6);
        assert!(soft_reach(2.0, 1.0, 0.04) <= 1.0);
        assert!(soft_reach(0.99, 1.0, 0.04) < 0.99);
        let mut prev = 0.0;
        for i in 0..200 {
            let r = soft_reach(i as f32 * 0.01, 1.0, 0.04);
            assert!(r >= prev);
            prev = r;
        }
    }
}
