//! A swinging foot is not caught by a transition, and a walk comes to rest at a path's end
//! without stopping dead (round 11a, docs/ANIMATION.md). Accelerations are in cm per 60 Hz
//! frame squared, as a game's checks count them.

use std::sync::Arc;

use loadngo_animation::controller::{Config, MotionMatcher};
use loadngo_animation::database::{
    BuildConfig, Database, LEFT_BALL, LEFT_HEEL, RIGHT_BALL, RIGHT_HEEL,
};
use loadngo_animation::math::Vec3;
use loadngo_animation::path::{ConstantSteering, PathSteering, Steering};
use loadngo_animation::testing::{
    humanoid_skeleton, synthetic_idle, synthetic_turn, synthetic_walk, synthetic_walk_to_stop,
};

fn db() -> Arc<Database> {
    let target = humanoid_skeleton(1.0);
    let clips = [
        synthetic_walk(&target, 600, 1.0),
        synthetic_walk(&target, 600, 0.7),
        synthetic_turn(&target, 300, 1.2),
        synthetic_walk_to_stop(&target, 300, 0.9, 1.5, 0.8),
        synthetic_idle(&target, 300),
    ];
    Arc::new(Database::build(&target, &clips, &BuildConfig::default()).unwrap())
}

fn arc(c: Vec3, r: f32, from: f32, to: f32, n: usize) -> Vec<Vec3> {
    (0..=n)
        .map(|i| {
            let a = from + (to - from) * i as f32 / n as f32;
            c + Vec3::new(r * a.cos(), 0.0, r * a.sin())
        })
        .collect()
}

const FEET: [u8; 2] = [LEFT_HEEL | LEFT_BALL, RIGHT_HEEL | RIGHT_BALL];

/// The largest acceleration of an ankle that was swinging (up, and not held) when a transition
/// was made, over the frame before, the frame of and the frame after it, on a walk along
/// `points`; and how many such ankles there were.
fn swing_across_transitions(
    db: &Arc<Database>,
    points: &[Vec3],
    speed: f32,
    hz: f32,
) -> (f32, usize) {
    let mut m = MotionMatcher::new(Arc::clone(db), Config::default(), points[0], 0.0);
    let path = PathSteering::new(points, speed);
    let dt = 1.0 / hz;
    let legs = db.humanoid.legs;
    let frames = (14.0 * hz) as usize;
    let mut ankles: Vec<[Vec3; 2]> = Vec::with_capacity(frames);
    let mut swinging: Vec<[bool; 2]> = Vec::with_capacity(frames);
    let mut transitioned = Vec::with_capacity(frames);
    for _ in 0..frames {
        let c = m.contacts();
        let held = m.feet_held();
        swinging.push([c & FEET[0] == 0 && !held[0], c & FEET[1] == 0 && !held[1]]);
        let t0 = m.stats().transitions;
        m.step(dt, &path);
        transitioned.push(m.stats().transitions != t0);
        let w = m.world_positions();
        ankles.push([w[legs[0].ankle], w[legs[1].ankle]]);
    }
    let scale = 100.0 * (hz / 60.0) * (hz / 60.0);
    let mut most = 0.0f32;
    let mut n = 0;
    for f in 2..frames - 2 {
        if !transitioned[f] {
            continue;
        }
        for side in 0..2 {
            if !swinging[f][side] {
                continue;
            }
            n += 1;
            for g in f - 1..=f + 1 {
                let a = (ankles[g + 1][side] - ankles[g][side] * 2.0 + ankles[g - 1][side])
                    .length()
                    * scale;
                most = most.max(a);
            }
        }
    }
    (most, n)
}

/// The synthetic clips end mid-stride, so walking a path crosses clip ends with a foot in the
/// air. A clip played to its end used to hold its last frame but one for a step and set off
/// again from rest: the swinging ankle 4.86 cm a frame squared at 60 Hz (loadngo `ec820357`);
/// 1.17 with playback carried to the last frame.
#[test]
fn a_clips_end_does_not_jolt_a_swinging_ankle() {
    let db = db();
    let z = |d: f32| Vec3::new(0.0, 0.0, d);
    for &r in &[0.6f32, 2.0] {
        for &dir in &[1.0f32, -1.0] {
            let mut p = vec![z(0.0), z(2.0)];
            let (a0, a1) = if dir > 0.0 {
                (std::f32::consts::PI, -std::f32::consts::PI)
            } else {
                (0.0, 2.0 * std::f32::consts::PI)
            };
            p.extend(
                arc(Vec3::new(dir * r, 0.0, 2.0), r, a0, a1, 40)
                    .into_iter()
                    .skip(1),
            );
            for (hz, limit) in [(60.0f32, 1.6f32), (30.0, 1.2)] {
                let (most, n) = swing_across_transitions(&db, &p, 0.8, hz);
                assert!(
                    n > 0,
                    "r {r} dir {dir} {hz} Hz: no transition with a foot swinging"
                );
                assert!(
                    most <= limit,
                    "r {r} dir {dir} {hz} Hz: a swinging ankle {most:.2} cm a frame squared across a transition (limit {limit})"
                );
            }
        }
    }
}

/// Transitions that put down a foot which was swinging faster than 0.6 m/s over the ground,
/// on a database whose pose features weigh almost nothing (so the search chooses frames by the
/// trajectory alone, whatever the feet are doing), steered through changes of speed and facing.
fn fast_feet_put_down(swing_agree: bool, hz: f32) -> usize {
    let target = humanoid_skeleton(1.0);
    let clips = [
        synthetic_walk(&target, 600, 1.0),
        synthetic_walk(&target, 600, 0.7),
        synthetic_turn(&target, 300, 1.2),
        synthetic_idle(&target, 300),
    ];
    let build = BuildConfig {
        weights: [0.01, 0.01, 1.0, 1.0, 1.5],
        ..BuildConfig::default()
    };
    let db = Arc::new(Database::build(&target, &clips, &build).unwrap());
    let legs = db.humanoid.legs;
    let dt = 1.0 / hz;
    let cfg = Config {
        swing_agree,
        ..Config::default()
    };
    let mut m = MotionMatcher::new(Arc::clone(&db), cfg, Vec3::ZERO, 0.0);
    let frames = (20.0 * hz) as usize;
    let mut ankles: Vec<[Vec3; 2]> = Vec::with_capacity(frames);
    let mut fast = 0;
    for i in 0..frames {
        let t = i as f32 * dt;
        let facing = 0.6 * (t * 0.8).sin();
        let speed = if ((t / 0.7) as usize).is_multiple_of(2) {
            1.0
        } else {
            0.7
        };
        let s = ConstantSteering {
            velocity: Vec3::new(facing.sin() * speed, 0.0, facing.cos() * speed),
            facing,
        };
        let c = m.contacts();
        let held = m.feet_held();
        let up = [c & FEET[0] == 0 && !held[0], c & FEET[1] == 0 && !held[1]];
        let t0 = m.stats().transitions;
        m.step(dt, &s);
        let w = m.world_positions();
        ankles.push([w[legs[0].ankle], w[legs[1].ankle]]);
        if m.stats().transitions != t0 && i >= 2 {
            let now = m.feet_held();
            for k in 0..2 {
                let v = (ankles[i - 1][k] - ankles[i - 2][k]).flat().length() * hz;
                if up[k] && now[k] && v > 0.6 {
                    fast += 1;
                }
            }
        }
    }
    fast
}

/// With the rule off this database puts a fast-swinging foot down at a transition (2 at 60 Hz, 1
/// at 30 Hz: 3.62 and 2.27 cm a frame squared at the ankle); with it on, none.
#[test]
fn a_transition_keeps_a_fast_swinging_foot_up() {
    for hz in [60.0f32, 30.0] {
        assert!(
            fast_feet_put_down(false, hz) > 0,
            "{hz} Hz: this database no longer tells the rule's effect"
        );
        assert_eq!(fast_feet_put_down(true, hz), 0, "{hz} Hz");
    }
}

/// A point that moves as a path wants comes to rest where the path is done: the speed wanted
/// falls a little each step, never a walk's pace to nothing in one. (Braking to the path's end
/// itself, the speed wanted was still 0.43-0.47 m/s at the arrive radius and nothing inside it:
/// a matched walk's root stopped dead there, and the head with it.)
#[test]
fn a_path_brakes_to_rest_where_it_is_done() {
    let z = |d: f32| Vec3::new(0.0, 0.0, d);
    for &len in &[1.5f32, 4.0] {
        for &speed in &[0.6f32, 1.0] {
            for hz in [60.0f32, 30.0] {
                let path = PathSteering::new(&[z(0.0), z(len)], speed);
                let dt = 1.0 / hz;
                let mut at = z(0.0);
                let mut was = speed;
                let mut drop = 0.0f32;
                for _ in 0..(12.0 * hz) as usize {
                    let v = path.desired(at).0;
                    drop = drop.max(was - v.length());
                    was = v.length();
                    at += v * dt;
                }
                assert!(
                    was == 0.0,
                    "len {len} v {speed} {hz} Hz: still wanted {was} m/s"
                );
                assert!(
                    (z(len) - at).length() <= path.arrive_radius + 1e-3,
                    "len {len} v {speed} {hz} Hz: rests {} m from the end",
                    (z(len) - at).length()
                );
                assert!(
                    drop <= 0.12,
                    "len {len} v {speed} {hz} Hz: the speed wanted fell {drop:.3} m/s in a step"
                );
            }
        }
    }
}
