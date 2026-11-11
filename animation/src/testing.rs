//! Fixtures for tests, benchmarks and examples: a Mixamo-named humanoid skeleton in a T
//! pose and a synthetic walk on it with planted feet. Not motion anyone should ship.

use crate::database::SourceClip;
use crate::humanoid::find_bone;
use crate::ik::two_bone;
use crate::math::{Quat, Transform, Vec3};
use crate::skeleton::{forward_kinematics, Skeleton};

/// A Mixamo-named humanoid in a T pose, facing +Z, standing on y = 0, its hips at
/// `scale` metres (about a 1.75 m adult at 1.0). Includes `HeadTop_End` and
/// `<side>Toe_End` leaves and one finger bone per hand, which motion matching does not drive.
#[must_use]
pub fn humanoid_skeleton(scale: f32) -> Skeleton {
    let mut names = Vec::new();
    let mut parents = Vec::new();
    let mut rest = Vec::new();
    let mut add = |n: &str, p: Option<usize>, t: Vec3| {
        names.push(n.to_string());
        parents.push(p);
        rest.push(Transform::new(t * scale, Quat::IDENTITY));
        names.len() - 1
    };
    let hips = add("Hips", None, Vec3::new(0.0, 1.0, 0.0));
    let spine = add("Spine", Some(hips), Vec3::new(0.0, 0.1, 0.0));
    let spine1 = add("Spine1", Some(spine), Vec3::new(0.0, 0.12, 0.0));
    let spine2 = add("Spine2", Some(spine1), Vec3::new(0.0, 0.13, 0.0));
    let neck = add("Neck", Some(spine2), Vec3::new(0.0, 0.15, 0.0));
    let head = add("Head", Some(neck), Vec3::new(0.0, 0.1, 0.02));
    let _ = add("HeadTop_End", Some(head), Vec3::new(0.0, 0.2, 0.0));
    for (side, s) in [("Left", 1.0f32), ("Right", -1.0)] {
        let sh = add(
            &format!("{side}Shoulder"),
            Some(spine2),
            Vec3::new(0.05 * s, 0.11, 0.0),
        );
        let arm = add(
            &format!("{side}Arm"),
            Some(sh),
            Vec3::new(0.12 * s, 0.0, 0.0),
        );
        let fore = add(
            &format!("{side}ForeArm"),
            Some(arm),
            Vec3::new(0.27 * s, 0.0, 0.0),
        );
        let hand = add(
            &format!("{side}Hand"),
            Some(fore),
            Vec3::new(0.25 * s, 0.0, 0.0),
        );
        let _ = add(
            &format!("{side}HandMiddle1"),
            Some(hand),
            Vec3::new(0.09 * s, 0.0, 0.0),
        );
    }
    for (side, s) in [("Left", 1.0f32), ("Right", -1.0)] {
        let up = add(
            &format!("{side}UpLeg"),
            Some(hips),
            Vec3::new(0.09 * s, -0.06, 0.0),
        );
        let knee = add(&format!("{side}Leg"), Some(up), Vec3::new(0.0, -0.44, 0.0));
        let ankle = add(
            &format!("{side}Foot"),
            Some(knee),
            Vec3::new(0.0, -0.42, 0.0),
        );
        let toe = add(
            &format!("{side}ToeBase"),
            Some(ankle),
            Vec3::new(0.0, -0.05, 0.13),
        );
        let _ = add(
            &format!("{side}Toe_End"),
            Some(toe),
            Vec3::new(0.0, 0.0, 0.06),
        );
    }
    Skeleton::new(names, parents, rest).expect("the fixture skeleton is valid")
}

/// A straight walk along +Z at `speed` m/s for `frames` frames at 60 fps on `target`
/// (from [`humanoid_skeleton`]): stance feet planted (two-bone IK), swing feet lifted, the
/// hips bobbing and swaying, arms down and swinging. A stride (two steps) lasts 1.2 s.
#[must_use]
#[allow(clippy::many_single_char_names)]
pub fn synthetic_walk(target: &Skeleton, frames: usize, speed: f32) -> SourceClip {
    let n = target.len();
    let f = |name: &str| find_bone(target, name).expect("fixture bone");
    let hips = f("Hips");
    let legs = [
        (f("LeftUpLeg"), f("LeftLeg"), f("LeftFoot")),
        (f("RightUpLeg"), f("RightLeg"), f("RightFoot")),
    ];
    let arms = [f("LeftArm"), f("RightArm")];
    let rest_world = target.rest_world();
    let hips_h = rest_world[hips].translation.y;
    let ankle_h = rest_world[legs[0].2].translation.y;
    let period = 1.2f32;
    let stride = speed * period;
    let mut out = Vec::with_capacity(frames * n);
    let mut local = target.rest.clone();
    let mut world = vec![Transform::IDENTITY; n];
    for i in 0..frames {
        let t = i as f32 / 60.0;
        let phase = t / period * std::f32::consts::TAU;
        local.clone_from(&target.rest);
        local[hips].translation = Vec3::new(
            0.02 * phase.sin(),
            hips_h - 0.03 + 0.015 * (2.0 * phase).cos(),
            speed * t,
        );
        local[hips].rotation = Quat::from_yaw(0.05 * phase.sin());
        for (k, &arm) in arms.iter().enumerate() {
            let side = if k == 0 { 1.0 } else { -1.0 };
            let swing = 0.3 * (phase + if k == 0 { std::f32::consts::PI } else { 0.0 }).sin();
            local[arm].rotation = Quat::from_axis_angle(Vec3::Z, -1.3 * side)
                * Quat::from_axis_angle(Vec3::Y, swing * side);
        }
        forward_kinematics(&target.parents, &local, Transform::IDENTITY, &mut world);
        for (k, &(up, knee, ankle)) in legs.iter().enumerate() {
            let offset = if k == 0 { 0.0 } else { 0.5 };
            let u_total = t / period + offset;
            let cycle = u_total.floor();
            let u = u_total - cycle;
            let x = world[up].translation.x;
            let z0 = 0.3 - stride * offset;
            let (z, lift) = if u < 0.6 {
                (cycle * stride + z0, 0.0)
            } else {
                let s = (u - 0.6) / 0.4;
                let e = s * s * (3.0 - 2.0 * s);
                (
                    cycle * stride + z0 + stride * e,
                    0.07 * (std::f32::consts::PI * s).sin(),
                )
            };
            let target_p = Vec3::new(x, ankle_h + lift, z);
            let sol = two_bone(
                world[up].translation,
                world[knee].translation,
                world[ankle].translation,
                target_p,
                Vec3::Z,
                0.02,
                world[up].rotation,
                world[knee].rotation,
            );
            // A slightly bent knee so the plane is defined.
            let parent = target.parents[up].map_or(Quat::IDENTITY, |p| world[p].rotation);
            local[up].rotation = (parent.inverse() * sol.root).normalize();
            local[knee].rotation = (sol.root.inverse() * sol.mid).normalize();
            // Feet flat on the ground.
            local[ankle].rotation = sol.mid.inverse();
            forward_kinematics(&target.parents, &local, Transform::IDENTITY, &mut world);
        }
        out.extend_from_slice(&local);
    }
    SourceClip {
        name: format!("synthetic-walk-{speed:.2}"),
        local: out,
    }
}

/// An in-place idle on `target`: hips still but for breathing, arms down.
#[must_use]
pub fn synthetic_idle(target: &Skeleton, frames: usize) -> SourceClip {
    let n = target.len();
    let hips = find_bone(target, "Hips").expect("fixture bone");
    let arms = [
        find_bone(target, "LeftArm").expect("fixture bone"),
        find_bone(target, "RightArm").expect("fixture bone"),
    ];
    let mut out = Vec::with_capacity(frames * n);
    for i in 0..frames {
        let t = i as f32 / 60.0;
        let mut local = target.rest.clone();
        local[hips].translation.y -= 0.01 + 0.003 * (t * 1.5).sin();
        local[arms[0]].rotation = Quat::from_axis_angle(Vec3::Z, -1.3);
        local[arms[1]].rotation = Quat::from_axis_angle(Vec3::Z, 1.3);
        out.extend_from_slice(&local);
    }
    SourceClip {
        name: "synthetic-idle".into(),
        local: out,
    }
}
