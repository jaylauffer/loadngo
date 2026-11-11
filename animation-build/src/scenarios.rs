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
    /// Sit on a seat: the seated hips (world; `y` their height) and the way they face. The
    /// leg ends when seated. Played through `character::Character`, not the bare matcher.
    Sit(Vec3, f32),
    /// Stand up from the seat; the leg ends when standing.
    Stand,
}

impl Scenario {
    /// Whether the scenario sits or stands (and so needs the character controller).
    #[must_use]
    pub fn uses_seats(&self) -> bool {
        self.legs
            .iter()
            .any(|l| matches!(l, Leg::Sit(..) | Leg::Stand))
    }
}

/// A named script.
#[derive(Debug, Clone)]
pub struct Scenario {
    pub name: String,
    pub about: String,
    pub legs: Vec<Leg>,
    /// Seconds of steady walking to measure the gait over: `(from, to)`; `None` for no
    /// gait measure.
    pub steady: Option<(f32, f32)>,
    /// Total seconds to run.
    pub seconds: f32,
    /// The pace the gait is judged against.
    pub pace: f32,
    /// Where the character starts, and its facing (radians about +Y, 0 = +Z).
    pub start: Vec3,
    pub facing: f32,
}

impl Scenario {
    /// A scenario from JSON: `{"name", "about", "points": [[x, z], ...], "speed", "facing",
    /// "wait_before", "wait_after", "steady": [from_s, to_s]}` (`facing`, the waits and
    /// `steady` optional). It starts standing at the first point.
    ///
    /// # Errors
    /// Malformed JSON or fewer than two points.
    pub fn from_json(bytes: &[u8]) -> Result<Self, String> {
        let v: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|e| format!("path JSON: {e}"))?;
        let f = |k: &str, d: f32| {
            v.get(k)
                .and_then(serde_json::Value::as_f64)
                .map_or(d, |x| x as f32)
        };
        let points: Vec<Vec3> = v
            .get("points")
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|p| {
                        let p = p.as_array()?;
                        Some(Vec3::new(
                            p.first()?.as_f64()? as f32,
                            0.0,
                            p.get(1)?.as_f64()? as f32,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if points.len() < 2 {
            return Err("a path needs at least two [x, z] points".into());
        }
        let speed = f("speed", STROLL);
        let before = f("wait_before", 1.0);
        let after = f("wait_after", 2.0);
        let length: f32 = points.windows(2).map(|w| (w[1] - w[0]).length()).sum();
        let d = points[1] - points[0];
        let steady = v
            .get("steady")
            .and_then(serde_json::Value::as_array)
            .and_then(|a| Some((a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32)));
        Ok(Self {
            name: v
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("path")
                .to_string(),
            about: v
                .get("about")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("a path from JSON")
                .to_string(),
            start: points[0],
            facing: f("facing", d.x.atan2(d.z)),
            seconds: before + length / speed.max(0.1) + 3.0 + after,
            legs: vec![
                Leg::Wait(before),
                Leg::Walk(points, speed),
                Leg::Wait(after),
            ],
            steady,
            pace: speed,
        })
    }
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
            name: "straight".into(),
            about: "12 m straight at a stroll (0.83 m/s), from standing to standing".into(),
            legs: vec![
                Leg::Wait(1.0),
                Leg::Walk(vec![z(0.0), z(12.0)], STROLL),
                Leg::Wait(2.0),
            ],
            steady: Some((4.0, 13.0)),
            seconds: 20.0,
            pace: STROLL,
            start: Vec3::ZERO,
            facing: 0.0,
        },
        Scenario {
            name: "start-stop".into(),
            about: "stand, walk 4 m, stop and stand 2 s, walk 4 m, stop".into(),
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
            start: Vec3::ZERO,
            facing: 0.0,
        },
        Scenario {
            name: "turn-left-90".into(),
            about: "6 m, a 90° turn to the left at a corner, 6 m".into(),
            legs: vec![
                Leg::Wait(1.0),
                Leg::Walk(vec![z(0.0), z(6.0), Vec3::new(6.0, 0.0, 6.0)], STROLL),
                Leg::Wait(2.0),
            ],
            steady: None,
            seconds: 19.0,
            pace: STROLL,
            start: Vec3::ZERO,
            facing: 0.0,
        },
        Scenario {
            name: "turn-right-90".into(),
            about: "6 m, a 90° turn to the right at a corner, 6 m".into(),
            legs: vec![
                Leg::Wait(1.0),
                Leg::Walk(vec![z(0.0), z(6.0), Vec3::new(-6.0, 0.0, 6.0)], STROLL),
                Leg::Wait(2.0),
            ],
            steady: None,
            seconds: 19.0,
            pace: STROLL,
            start: Vec3::ZERO,
            facing: 0.0,
        },
        Scenario {
            name: "curve".into(),
            about: "a gentle curve: a quarter circle of 5 m radius after 2 m straight".into(),
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
            start: Vec3::ZERO,
            facing: 0.0,
        },
        Scenario {
            name: "turn-off-80".into(),
            about: "standing facing +Z, then off along a path 80 degrees to the left: the turn from standing as one rises and goes".into(),
            legs: vec![
                Leg::Wait(1.0),
                Leg::Walk(vec![Vec3::ZERO, Vec3::new(5.0 * 80f32.to_radians().sin(), 0.0, 5.0 * 80f32.to_radians().cos())], STROLL),
                Leg::Wait(2.0),
            ],
            steady: None,
            seconds: 12.0,
            pace: STROLL,
            start: Vec3::ZERO,
            facing: 0.0,
        },
        Scenario {
            name: "turn-off-180".into(),
            about: "standing facing +Z, then off the way they came: a turn about from standing".into(),
            legs: vec![Leg::Wait(1.0), Leg::Walk(vec![Vec3::ZERO, Vec3::new(0.0, 0.0, -5.0)], STROLL), Leg::Wait(2.0)],
            steady: None,
            seconds: 12.0,
            pace: STROLL,
            start: Vec3::ZERO,
            facing: 0.0,
        },
        Scenario {
            name: "alongside-slow".into(),
            about: "12 m straight at 0.6 m/s, keeping pace with someone slower".into(),
            legs: vec![
                Leg::Wait(1.0),
                Leg::Walk(vec![z(0.0), z(12.0)], 0.6),
                Leg::Wait(2.0),
            ],
            steady: Some((5.0, 18.0)),
            seconds: 25.0,
            pace: 0.6,
            start: Vec3::ZERO,
            facing: 0.0,
        },
        Scenario {
            name: "sit-bench".into(),
            about: "walk 3 m to a bench, turn and sit (hips 0.45 m up), sit 4 s, stand, walk 2 m away".into(),
            legs: vec![
                Leg::Wait(0.5),
                Leg::Sit(Vec3::new(0.6, 0.45, 3.2), -std::f32::consts::FRAC_PI_2),
                Leg::Wait(4.0),
                Leg::Stand,
                Leg::Walk(vec![Vec3::new(0.6, 0.0, 3.2), Vec3::new(-1.4, 0.0, 3.2)], STROLL),
                Leg::Wait(1.0),
            ],
            steady: None,
            seconds: 22.0,
            pace: STROLL,
            start: Vec3::ZERO,
            facing: 0.0,
        },
        Scenario {
            name: "sit-chair".into(),
            about: "a chair 2.5 m ahead facing the walker (hips 0.5 m up): walk up, turn about, sit 3 s, stand".into(),
            legs: vec![
                Leg::Wait(0.5),
                Leg::Sit(Vec3::new(0.0, 0.5, 2.5), std::f32::consts::PI),
                Leg::Wait(3.0),
                Leg::Stand,
                Leg::Wait(1.5),
            ],
            steady: None,
            seconds: 18.0,
            pace: STROLL,
            start: Vec3::ZERO,
            facing: 0.0,
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
            Some(Leg::Sit(..) | Leg::Stand) => true,
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
