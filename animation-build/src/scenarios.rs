//! Scripted walks for the evaluation harness: paths at a pace, with waits between them.

use loadngo_animation::math::Vec3;
use loadngo_animation::path::{PathSteering, Steering};

/// A real stroll (about 88 steps a minute of 0.55 m).
pub const STROLL: f32 = 0.83;

/// One part of a script.
#[derive(Debug, Clone)]
pub enum Leg {
    /// Walk this polyline at this speed, stopping at its end.
    Walk(Vec<Vec3>, f32),
    /// Stand for this many seconds.
    Wait(f32),
}

/// A named script.
#[derive(Debug, Clone)]
pub struct Scenario {
    pub name: &'static str,
    pub about: &'static str,
    pub legs: Vec<Leg>,
    /// Seconds of steady walking to measure the gait over: `(from, to)`; `None` for no
    /// gait measure.
    pub steady: Option<(f32, f32)>,
    /// Total seconds to run.
    pub seconds: f32,
    /// The pace the gait is judged against.
    pub pace: f32,
}

fn arc(center: Vec3, radius: f32, from: f32, to: f32, n: usize) -> Vec<Vec3> {
    (0..=n)
        .map(|i| {
            let a = from + (to - from) * i as f32 / n as f32;
            center + Vec3::new(radius * a.cos(), 0.0, radius * a.sin())
        })
        .collect()
}

/// The standard set: straight, start and stop, 90° turns both ways, a gentle curve, and a
/// slower walk alongside someone.
#[must_use]
pub fn standard() -> Vec<Scenario> {
    let z = |d: f32| Vec3::new(0.0, 0.0, d);
    vec![
        Scenario {
            name: "straight",
            about: "12 m straight at a stroll (0.83 m/s), from standing to standing",
            legs: vec![
                Leg::Wait(1.0),
                Leg::Walk(vec![z(0.0), z(12.0)], STROLL),
                Leg::Wait(2.0),
            ],
            steady: Some((4.0, 13.0)),
            seconds: 20.0,
            pace: STROLL,
        },
        Scenario {
            name: "start-stop",
            about: "stand, walk 4 m, stop and stand 2 s, walk 4 m, stop",
            legs: vec![
                Leg::Wait(1.5),
                Leg::Walk(vec![z(0.0), z(4.0)], STROLL),
                Leg::Wait(2.0),
                Leg::Walk(vec![z(4.0), z(8.0)], STROLL),
                Leg::Wait(2.0),
            ],
            steady: None,
            seconds: 19.0,
            pace: STROLL,
        },
        Scenario {
            name: "turn-left-90",
            about: "6 m, a 90° turn to the left at a corner, 6 m",
            legs: vec![
                Leg::Wait(1.0),
                Leg::Walk(vec![z(0.0), z(6.0), Vec3::new(6.0, 0.0, 6.0)], STROLL),
                Leg::Wait(2.0),
            ],
            steady: None,
            seconds: 19.0,
            pace: STROLL,
        },
        Scenario {
            name: "turn-right-90",
            about: "6 m, a 90° turn to the right at a corner, 6 m",
            legs: vec![
                Leg::Wait(1.0),
                Leg::Walk(vec![z(0.0), z(6.0), Vec3::new(-6.0, 0.0, 6.0)], STROLL),
                Leg::Wait(2.0),
            ],
            steady: None,
            seconds: 19.0,
            pace: STROLL,
        },
        Scenario {
            name: "curve",
            about: "a gentle curve: a quarter circle of 5 m radius after 2 m straight",
            legs: vec![
                Leg::Wait(1.0),
                Leg::Walk(
                    {
                        let mut p = vec![z(0.0), z(2.0)];
                        p.extend(
                            arc(
                                Vec3::new(5.0, 0.0, 2.0),
                                5.0,
                                std::f32::consts::PI,
                                std::f32::consts::FRAC_PI_2,
                                24,
                            )
                            .into_iter()
                            .skip(1),
                        );
                        p
                    },
                    STROLL,
                ),
                Leg::Wait(2.0),
            ],
            steady: Some((4.0, 11.0)),
            seconds: 18.0,
            pace: STROLL,
        },
        Scenario {
            name: "alongside-slow",
            about: "12 m straight at 0.6 m/s, keeping pace with someone slower",
            legs: vec![
                Leg::Wait(1.0),
                Leg::Walk(vec![z(0.0), z(12.0)], 0.6),
                Leg::Wait(2.0),
            ],
            steady: Some((5.0, 18.0)),
            seconds: 25.0,
            pace: 0.6,
        },
    ]
}

/// Plays a scenario's legs in turn: [`Script::update`] each frame, then steer by it.
#[derive(Debug, Clone)]
pub struct Script {
    legs: Vec<Leg>,
    at: usize,
    leg_time: f32,
    current: Option<PathSteering>,
    hold_facing: f32,
}

impl Script {
    #[must_use]
    pub fn new(s: &Scenario) -> Self {
        let mut me = Self {
            legs: s.legs.clone(),
            at: 0,
            leg_time: 0.0,
            current: None,
            hold_facing: 0.0,
        };
        me.enter();
        me
    }

    fn enter(&mut self) {
        self.leg_time = 0.0;
        self.current = match self.legs.get(self.at) {
            Some(Leg::Walk(p, speed)) => Some(PathSteering::new(p, *speed)),
            _ => None,
        };
    }

    /// Which leg is playing.
    #[must_use]
    pub fn leg(&self) -> usize {
        self.at
    }

    /// Advances by `dt` given where the simulated character is.
    pub fn update(&mut self, sim_position: Vec3, sim_speed: f32, dt: f32) {
        self.leg_time += dt;
        let next = match self.legs.get(self.at) {
            Some(Leg::Wait(s)) => self.leg_time >= *s,
            Some(Leg::Walk(p, _)) => {
                let end = *p.last().unwrap_or(&Vec3::ZERO);
                let (v, _) = self
                    .current
                    .as_ref()
                    .map_or((Vec3::ZERO, 0.0), |c| c.desired(sim_position));
                let arrived = v.length() < 1e-6 && sim_speed < 0.05;
                if arrived {
                    if let Some(c) = &self.current {
                        let (_, f) = c.desired(end);
                        self.hold_facing = f;
                    }
                }
                arrived
            }
            None => false,
        };
        if next && self.at + 1 < self.legs.len() {
            self.at += 1;
            self.enter();
        }
    }
}

impl Steering for Script {
    fn desired(&self, position: Vec3) -> (Vec3, f32) {
        match &self.current {
            Some(p) => p.desired(position),
            None => (Vec3::ZERO, self.hold_facing),
        }
    }
}
