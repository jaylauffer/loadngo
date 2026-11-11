//! A swinging foot is not caught by a transition, and a walk comes to rest at a path's end
//! without stopping dead (round 11a).

use std::sync::Arc;

use loadngo_animation::controller::{Config, MotionMatcher};
use loadngo_animation::database::{
    BuildConfig, Database, LEFT_BALL, LEFT_HEEL, RIGHT_BALL, RIGHT_HEEL,
};
use loadngo_animation::math::Vec3;
use loadngo_animation::path::PathSteering;
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

/// Per 60 Hz frame, cm.
fn swing_report(db: &Arc<Database>, points: &[Vec3], speed: f32, hz: f32) -> (f32, usize, u64) {
    let cfg = Config {
        swing_agree: std::env::var_os("SWING_OFF").is_none(),
        ..Config::default()
    };
    let mut m = MotionMatcher::new(Arc::clone(db), cfg, points[0], 0.0);
    let path = PathSteering::new(points, speed);
    let dt = 1.0 / hz;
    let legs = db.humanoid.legs;
    let bits = [LEFT_HEEL | LEFT_BALL, RIGHT_HEEL | RIGHT_BALL];
    let frames = (14.0 * hz) as usize;
    let mut ankles: Vec<[Vec3; 2]> = Vec::with_capacity(frames);
    let mut swinging: Vec<[bool; 2]> = Vec::with_capacity(frames);
    let mut transitioned = Vec::with_capacity(frames);
    for _ in 0..frames {
        let c = m.contacts();
        let held = m.feet_held();
        swinging.push([c & bits[0] == 0 && !held[0], c & bits[1] == 0 && !held[1]]);
        let t0 = m.stats().transitions;
        m.step(dt, &path);
        transitioned.push(m.stats().transitions != t0);
        let w = m.world_positions();
        ankles.push([w[legs[0].ankle], w[legs[1].ankle]]);
    }
    let scale =
        100.0 * (60.0 / hz) * (60.0 / hz) / ((60.0 / hz) * (60.0 / hz)) * (hz / 60.0) * (hz / 60.0);
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
    (most, n, m.stats().transitions)
}

#[test]
fn diag_swing() {
    let db = db();
    let z = |d: f32| Vec3::new(0.0, 0.0, d);
    for &r in &[0.6f32, 0.8, 1.2, 2.0] {
        for &dir in &[1.0f32, -1.0] {
            for &speed in &[0.8f32, 1.0] {
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
                for hz in [60.0f32, 30.0] {
                    let (most, n, t) = swing_report(&db, &p, speed, hz);
                    println!("r {r} dir {dir} v {speed} {hz} Hz: swing ankle acc at transitions {most:.2} over {n} swinging feet, {t} transitions");
                }
            }
        }
    }
}

#[test]
fn diag_let_go() {
    use loadngo_animation::path::ConstantSteering;
    let db = db();
    let legs = db.humanoid.legs;
    let bits = [LEFT_HEEL | LEFT_BALL, RIGHT_HEEL | RIGHT_BALL];
    for hz in [60.0f32, 30.0] {
        let dt = 1.0 / hz;
        let mut worst = 0.0f32;
        let mut grabs = 0;
        for k in 0..30 {
            let cfg = Config {
                swing_agree: std::env::var_os("SWING_OFF").is_none(),
                swing_speed: 0.6,
                swing_mode: 3.0,
                ..Config::default()
            };
            let mut m = MotionMatcher::new(Arc::clone(&db), cfg, Vec3::ZERO, 0.0);
            let go = ConstantSteering {
                velocity: Vec3::new(0.0, 0.0, 1.0),
                facing: 0.0,
            };
            let stop = ConstantSteering {
                velocity: Vec3::ZERO,
                facing: 0.0,
            };
            let let_go = 3.0 + k as f32 * 0.04;
            let frames = (6.0 * hz) as usize;
            let mut ankles = Vec::new();
            let mut ev = Vec::new();
            for i in 0..frames {
                let t = i as f32 * dt;
                let c = m.contacts();
                let held = m.feet_held();
                let sw = [c & bits[0] == 0 && !held[0], c & bits[1] == 0 && !held[1]];
                let t0 = m.stats().transitions;
                if t < let_go {
                    m.step(dt, &go)
                } else {
                    m.step(dt, &stop)
                }
                let w = m.world_positions();
                ankles.push([w[legs[0].ankle], w[legs[1].ankle]]);
                let h = m.feet_held();
                if m.stats().transitions != t0 {
                    for s in 0..2 {
                        if sw[s] && h[s] {
                            ev.push((i, s));
                        }
                    }
                }
            }
            let sa = (hz / 60.0) * (hz / 60.0) * 100.0;
            for &(i, s) in &ev {
                if i < 2 || i + 2 >= frames {
                    continue;
                }
                let v = (ankles[i - 1][s] - ankles[i - 2][s]).length() * 100.0 * 60.0 / hz;
                if v > 1.0 {
                    grabs += 1;
                }
                for g in i - 1..=i + 1 {
                    worst = worst.max(
                        (ankles[g + 1][s] - ankles[g][s] * 2.0 + ankles[g - 1][s]).length() * sa,
                    );
                }
            }
        }
        println!("{hz} Hz: swinging feet put down by a transition {grabs} (moving over 1 cm a frame), worst ankle acc there {worst:.2}");
    }
}

#[test]
fn diag_blind() {
    use loadngo_animation::path::ConstantSteering;
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
    let bits = [LEFT_HEEL | LEFT_BALL, RIGHT_HEEL | RIGHT_BALL];
    for hz in [60.0f32, 30.0] {
        let dt = 1.0 / hz;
        let mut cfg = Config::default();
        if std::env::var_os("SWING_OFF").is_some() {
            cfg.set("swing_agree", "false").unwrap();
        }
        let mut m = MotionMatcher::new(Arc::clone(&db), cfg, Vec3::ZERO, 0.0);
        let frames = (20.0 * hz) as usize;
        let mut ankles: Vec<[Vec3; 2]> = Vec::new();
        let mut ev = Vec::new();
        for i in 0..frames {
            let t = i as f32 * dt;
            let fast = (t / 0.7) as usize % 2 == 0;
            let facing = 0.6 * (t * 0.8).sin();
            let speed = if fast { 1.0 } else { 0.7 };
            let s = ConstantSteering {
                velocity: Vec3::new(facing.sin() * speed, 0.0, facing.cos() * speed),
                facing,
            };
            let c = m.contacts();
            let held = m.feet_held();
            let sw = [c & bits[0] == 0 && !held[0], c & bits[1] == 0 && !held[1]];
            let t0 = m.stats().transitions;
            m.step(dt, &s);
            let w = m.world_positions();
            ankles.push([w[legs[0].ankle], w[legs[1].ankle]]);
            let h = m.feet_held();
            if m.stats().transitions != t0 {
                for k in 0..2 {
                    if sw[k] && h[k] {
                        ev.push((i, k));
                        println!(
                            "   ev {:.3}s side {k} frame {} contacts {:04b} unstuck {}",
                            t,
                            m.frame(),
                            m.contacts(),
                            m.stats().unstuck
                        );
                    }
                }
            }
        }
        let sa = (hz / 60.0) * (hz / 60.0) * 100.0;
        let mut worst = 0.0f32;
        let mut fast_grabs = 0;
        for &(i, k) in &ev {
            if i < 2 || i + 2 >= frames {
                continue;
            }
            let v = (ankles[i - 1][k] - ankles[i - 2][k]).flat().length() * hz;
            if v > 0.6 {
                fast_grabs += 1;
            }
            for g in i - 1..=i + 1 {
                worst = worst
                    .max((ankles[g + 1][k] - ankles[g][k] * 2.0 + ankles[g - 1][k]).length() * sa);
            }
        }
        println!("{hz} Hz: {} transitions; swinging feet put down by one {} ({fast_grabs} moving over 0.6 m/s); worst ankle acc there {worst:.2}", m.stats().transitions, ev.len());
    }
}
