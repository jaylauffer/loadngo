//! The character controller a game drives: commands over the motion matcher, and layers on
//! its pose.
//!
//! **Commands.** [`Character::go_to`] a path (stopping facing a way), [`Character::face`] a
//! direction, [`Character::follow`] someone at an offset (call each frame), [`Character::idle`],
//! [`Character::sit`] on a [`Seat`] and [`Character::stand`] up, [`Character::gesture`] on word
//! timings, [`Character::look_at`] a point, [`Character::hand_to`] a surface or prop. A command
//! that needs the character standing (a walk while seated) stands it up first.
//!
//! **Base.** Motion matching over the locomotion clips (walks, turns, starts, stops, idles);
//! sitting down, the seated loop and standing up are clips played as actions
//! ([`crate::controller::Action`]), entered and left by inertialization, the sit warped so the
//! hips land on the seat.
//!
//! **Layers**, applied to the base pose each step, in order: beat gestures (the forearm and hand
//! of one arm, and a nod, on each word), look-at (spread over the chest, neck and head, within
//! limits and eased), hand IK (two-bone, the arm only). The legs and feet are the base's, held
//! by its foot locking; the face, eyes and every morph target are never touched.
//!
//! [`Character::step`] allocates nothing.

use std::sync::Arc;

use crate::controller::{Action, Config, MotionMatcher};
use crate::database::{tags, Database};
use crate::humanoid::find_bone;
use crate::ik::two_bone;
use crate::math::{wrap_angle, Quat, Transform, Vec3};
use crate::path::{ConstantSteering, PathSteering, Steering, MAX_PATH_POINTS};

/// A seat: where the seated hips go (world, metres; `y` their height over the floor) and the
/// way the sitter faces (radians about +Y, 0 = +Z).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seat {
    pub hips: Vec3,
    pub facing: f32,
}

/// What the character is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Standing,
    Walking,
    /// Walking to the spot in front of a seat.
    Approaching,
    SittingDown,
    Seated,
    StandingUp,
}

/// Follows someone: their position, velocity and heading now, and where to be relative to
/// them (`x` to their right, `z` ahead of them, metres).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Follow {
    pub position: Vec3,
    pub velocity: Vec3,
    pub heading: f32,
    pub offset: Vec3,
}

impl Steering for Follow {
    fn desired(&self, p: Vec3) -> (Vec3, f32) {
        let (s, c) = self.heading.sin_cos();
        let right = Vec3::new(-c, 0.0, s);
        let ahead = Vec3::new(s, 0.0, c);
        let spot = self.position + right * self.offset.x + ahead * self.offset.z;
        let to = (spot - p).flat();
        // Their pace, plus closing the gap within about a second, at most a brisk walk.
        let mut v = self.velocity.flat() + to * 1.2;
        let speed = v.length();
        if speed > 1.4 {
            v *= 1.4 / speed;
        }
        if v.length() < 0.08 && to.length() < 0.15 {
            return (Vec3::ZERO, self.heading);
        }
        let facing = if v.length() > 0.25 {
            v.x.atan2(v.z)
        } else {
            self.heading
        };
        (v, facing)
    }
}

enum Steer {
    Constant(ConstantSteering),
    Path(Box<PathSteering>),
    Follow(Follow),
}

impl Steering for Steer {
    fn desired(&self, p: Vec3) -> (Vec3, f32) {
        match self {
            Self::Constant(c) => c.desired(p),
            Self::Path(s) => s.desired(p),
            Self::Follow(f) => f.desired(p),
        }
    }
}

const MAX_BEATS: usize = 64;

/// How far from the sit's starting spot (m) a settled character may begin the sit, the
/// clip's warp taking up the rest.
const APPROACH_SLACK: f32 = 0.35;

/// How far (m) a half turn in place carries a character forward, in the capture: the walk
/// to a seat stops this much short (scaled by the turn) so that turning round lands on it.
const TURN_LEAD: f32 = 0.3;

#[derive(Debug, Clone, Copy)]
struct Arm {
    shoulder: usize,
    arm: usize,
    fore: usize,
    hand: usize,
}

/// One character.
pub struct Character {
    m: MotionMatcher,
    db: Arc<Database>,
    steer: Steer,
    after_standing: Option<Steer>,
    state: State,
    seat: Option<Seat>,
    sit: Option<Action>,
    hold: Option<(usize, usize)>,
    approach: Transform,
    last_root: Vec3,
    settled: f32,
    clock: f32,
    beats: [f32; MAX_BEATS],
    beat_count: usize,
    beat_strength: f32,
    look: Option<Vec3>,
    look_weight: f32,
    look_angles: (f32, f32),
    hands: [Option<Vec3>; 2],
    hand_weights: [f32; 2],
    spine2: Option<usize>,
    neck: Option<usize>,
    head: Option<usize>,
    arms: [Option<Arm>; 2],
    rot: Vec<Quat>,
    world_rot: Vec<Quat>,
    world_pos: Vec<Vec3>,
    local_pos: Vec<Vec3>,
}

/// The feet's places relative to the hips (on the root's axes), from the features.
fn feet_from_hips(db: &Database, f: usize) -> [Vec3; 2] {
    let v = db.feature(f);
    let at = |k: usize| v[k] * db.feature_scale[k] + db.feature_mean[k];
    let h = db.hips_pos[f];
    [
        Vec3::new(at(0), at(1), at(2)) - h,
        Vec3::new(at(3), at(4), at(5)) - h,
    ]
}

/// How far (m) the feet would move if frame `b`'s hips were put where frame `a`'s are.
fn seat_distance(db: &Database, a: usize, b: usize) -> f32 {
    let (pa, pb) = (feet_from_hips(db, a), feet_from_hips(db, b));
    (pa[0] - pb[0]).length() + (pa[1] - pb[1]).length()
}

fn smooth_to(current: f32, target: f32, halflife: f32, dt: f32) -> f32 {
    current + (target - current) * (1.0 - (-std::f32::consts::LN_2 * dt / halflife.max(1e-4)).exp())
}

impl Character {
    /// A character standing at `position` facing `facing`.
    #[must_use]
    pub fn new(db: Arc<Database>, config: Config, position: Vec3, facing: f32) -> Self {
        let n = db.bone_count();
        let sk = &db.skeleton;
        let arm = |side: &str| {
            Some(Arm {
                shoulder: find_bone(sk, &format!("{side}Shoulder"))?,
                arm: find_bone(sk, &format!("{side}Arm"))?,
                fore: find_bone(sk, &format!("{side}ForeArm"))?,
                hand: find_bone(sk, &format!("{side}Hand"))?,
            })
        };
        let local_pos = sk.rest.iter().map(|t| t.translation).collect();
        Self {
            m: MotionMatcher::new(Arc::clone(&db), config, position, facing),
            steer: Steer::Constant(ConstantSteering {
                velocity: Vec3::ZERO,
                facing,
            }),
            after_standing: None,
            state: State::Standing,
            seat: None,
            sit: None,
            hold: None,
            approach: Transform::IDENTITY,
            last_root: position,
            settled: 0.0,
            clock: 0.0,
            beats: [0.0; MAX_BEATS],
            beat_count: 0,
            beat_strength: 1.0,
            look: None,
            look_weight: 0.0,
            look_angles: (0.0, 0.0),
            hands: [None; 2],
            hand_weights: [0.0; 2],
            spine2: find_bone(sk, "Spine2"),
            neck: find_bone(sk, "Neck"),
            head: find_bone(sk, "Head"),
            arms: [arm("Left"), arm("Right")],
            rot: vec![Quat::IDENTITY; n],
            world_rot: vec![Quat::IDENTITY; n],
            world_pos: vec![Vec3::ZERO; n],
            local_pos,
            db,
        }
    }

    /// Where a sit begins: the spot and facing the chosen sitting-down clip starts from
    /// (meaningful while approaching a seat). A host routing round obstacles aims here.
    #[must_use]
    pub fn approach_spot(&self) -> Transform {
        self.approach
    }

    /// The matcher underneath (its settings, stats and feet).
    #[must_use]
    pub fn matcher(&self) -> &MotionMatcher {
        &self.m
    }

    /// Changes the matcher's settings.
    pub fn set_config(&mut self, config: Config) {
        self.m.set_config(config);
    }

    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    fn standing_or_queue(&mut self, steer: Steer) {
        // Any command but a sit forgets a sit not yet begun, and a seat being left.
        self.sit = None;
        if matches!(self.state, State::SittingDown | State::Seated) {
            self.seat = None;
        }
        match self.state {
            State::Seated | State::SittingDown => {
                self.after_standing = Some(steer);
                self.stand();
            }
            State::StandingUp => self.after_standing = Some(steer),
            _ => {
                self.steer = steer;
                self.state = State::Walking;
                self.seat = None;
                self.sit = None;
            }
        }
    }

    /// Walks a path through `points` (the first is usually where it stands) at `speed` m/s,
    /// stopping at its end facing `end_facing` (or the last leg's way).
    pub fn go_to(&mut self, points: &[Vec3], speed: f32, end_facing: Option<f32>) {
        let mut p = PathSteering::new(&points[..points.len().min(MAX_PATH_POINTS)], speed);
        p.end_facing = end_facing;
        self.standing_or_queue(Steer::Path(Box::new(p)));
    }

    /// Stands and turns to face `facing`.
    pub fn face(&mut self, facing: f32) {
        self.standing_or_queue(Steer::Constant(ConstantSteering {
            velocity: Vec3::ZERO,
            facing,
        }));
    }

    /// Keeps to `follow.offset` from someone; call each frame with where they are.
    pub fn follow(&mut self, follow: Follow) {
        self.standing_or_queue(Steer::Follow(follow));
    }

    /// Steers by a ground velocity (m/s) and a facing: a stick, or a host that steers itself.
    pub fn steer(&mut self, velocity: Vec3, facing: f32) {
        self.standing_or_queue(Steer::Constant(ConstantSteering { velocity, facing }));
    }

    /// Places the character at once, standing (a cut).
    pub fn teleport(&mut self, position: Vec3, facing: f32) {
        self.m.stop_action();
        self.m.teleport(position, facing);
        self.state = State::Standing;
        self.sit = None;
        self.seat = None;
        self.after_standing = None;
        self.steer = Steer::Constant(ConstantSteering {
            velocity: Vec3::ZERO,
            facing,
        });
    }

    /// Plants the feet where they stand now (each ankle's place on the ground, world; `None`
    /// leaves a foot to the capture): for a body handed over from other animation
    /// (`MotionMatcher::plant_feet`).
    pub fn plant_feet(&mut self, ankles: [Option<Vec3>; 2]) {
        self.m.plant_feet(ankles);
    }

    /// Stops and stands about where it is (idles).
    pub fn idle(&mut self) {
        let facing = self.m.root().rotation.yaw();
        self.face(facing);
    }

    fn clip_ok(&self, clip_tags: u32, want: u32) -> bool {
        clip_tags & want != 0 && clip_tags & self.m.config().exclude_tags == 0
    }

    /// Sits on `seat`: walks to the spot in front of it the sitting clip starts from, turns
    /// its back to the seat, and sits, the hips landing on `seat.hips`. Then a seated loop.
    pub fn sit(&mut self, seat: Seat) {
        let db = Arc::clone(&self.db);
        // The sitting-down clip and the seated loop to follow it, chosen together: the sit's
        // seated hips near the seat's height, and the loop's feet near where the sit leaves
        // them (a capture's own sit and seated stretch, where the data has both).
        let mut best: Option<(f32, usize, usize, (usize, usize))> = None;
        for c in &db.clips {
            if !self.clip_ok(c.tags, tags::SIT_DOWN) {
                continue;
            }
            let h = db.hips_pos[c.end - 1].y;
            let Some((feet, hold)) = self.seated_after(c.end - 1) else {
                continue;
            };
            let d = 2.0 * (h - seat.hips.y).abs() + feet;
            if best.is_none_or(|b| d < b.0) {
                best = Some((d, c.start, c.end, hold));
            }
        }
        let Some((_, start, end, hold)) = best else {
            return;
        };
        self.hold = Some(hold);
        let last = end - 1;
        let facing = Quat::from_yaw(seat.facing);
        let hips_end = db.hips_pos[last];
        let root_end = Transform::new(seat.hips.flat() - facing.rotate(hips_end.flat()), facing);
        let db_end = Transform::new(db.root_pos[last], db.root_rot[last]);
        let db_start = Transform::new(db.root_pos[start], db.root_rot[start]);
        self.approach = root_end.compose(db_end.inverse()).compose(db_start);
        self.sit = Some(Action {
            start,
            end,
            looping: false,
            pin_root: false,
            root_target: Some(root_end),
            hips_height_target: Some(seat.hips.y),
        });
        self.seat = Some(seat);
        // Stop short of the spot along the way in, by about what turning round there will
        // carry the character on (the capture's turns in place step forward as they turn).
        let here = self.m.root().translation;
        let spot = self.approach.translation;
        let way_in = (spot - here).flat();
        let dir = way_in.normalize_or_zero();
        let turn = wrap_angle(self.approach.rotation.yaw() - dir.x.atan2(dir.z)).abs();
        let to = spot - dir * (TURN_LEAD * turn / std::f32::consts::PI).min(way_in.length());
        let mut p = PathSteering::new(&[here, to], 0.7);
        p.end_facing = Some(self.approach.rotation.yaw());
        p.arrive_radius = 0.05;
        if matches!(
            self.state,
            State::Seated | State::SittingDown | State::StandingUp
        ) {
            self.after_standing = Some(Steer::Path(Box::new(p)));
            self.stand();
            self.state = State::StandingUp;
        } else {
            self.steer = Steer::Path(Box::new(p));
            self.state = State::Approaching;
        }
    }

    /// Stands up from the seat (and then does whatever was asked meanwhile).
    pub fn stand(&mut self) {
        if !matches!(self.state, State::Seated | State::SittingDown) {
            return;
        }
        let db = Arc::clone(&self.db);
        let hips_now = self.m.hips_position().y;
        let now = self.m.frame();
        // The rising frame (in a clip's first half second) whose feet, from the hips, are
        // nearest the pose held now, at about the height the hips are.
        let mut best: Option<(f32, usize, usize)> = None;
        for c in &db.clips {
            if !self.clip_ok(c.tags, tags::STAND_UP) {
                continue;
            }
            let lead = ((0.5 * db.fps) as usize).min((c.end - c.start) / 3);
            for f in (c.start..=c.start + lead).step_by(4) {
                let d = seat_distance(&db, now, f)
                    + 2.0 * (db.hips_pos[f].y + self.m.hips_lift() - hips_now).abs();
                if best.is_none_or(|b| d < b.0) {
                    best = Some((d, f, c.end));
                }
            }
        }
        let Some((_, start, end)) = best else { return };
        self.m.play(Action {
            start,
            end,
            looping: false,
            pin_root: false,
            root_target: None,
            hips_height_target: None,
        });
        self.state = State::StandingUp;
    }

    /// Beat gestures on these word times (seconds from now), at `strength` (1 = a speaker's
    /// ordinary beats). Later calls add to those still to come.
    pub fn gesture(&mut self, word_times: &[f32], strength: f32) {
        self.beat_strength = strength.clamp(0.0, 2.0);
        for t in word_times {
            if self.beat_count < MAX_BEATS {
                self.beats[self.beat_count] = self.clock + t;
                self.beat_count += 1;
            }
        }
    }

    /// Turns the chest, neck and head towards `target` (world), or back with `None`.
    pub fn look_at(&mut self, target: Option<Vec3>) {
        self.look = target;
    }

    /// Reaches a hand (0 left, 1 right) to `target` (the wrist, world), or lets it go.
    pub fn hand_to(&mut self, side: usize, target: Option<Vec3>) {
        if side < 2 {
            self.hands[side] = target;
        }
    }

    /// Advances by `dt` seconds.
    pub fn step(&mut self, dt: f32) {
        let dt = dt.clamp(1e-4, 0.1);
        self.clock += dt;
        // The seat's sequence.
        match self.state {
            State::Approaching => {
                // Sit once at the spot and turned; or, turned and near, once the feet have
                // settled (small corrective steps look worse than the sit's own warp, which
                // takes up what is left over the clip's length).
                let r = self.m.root();
                let off = (r.translation - self.approach.translation).flat().length();
                let turned =
                    wrap_angle(r.rotation.yaw() - self.approach.rotation.yaw()).abs() < 0.45;
                let moving = (r.translation - self.last_root).flat().length() / dt > 0.12;
                self.settled = if moving { 0.0 } else { self.settled + dt };
                if turned && (off < 0.12 || (off < APPROACH_SLACK && self.settled > 0.3)) {
                    if let Some(a) = self.sit {
                        self.m.play(a);
                        self.state = State::SittingDown;
                    }
                }
            }
            State::Walking => {
                let (v, _) = self.steer.desired(self.m.root().translation);
                if v.length() < 1e-6 && self.m.action().is_none() {
                    self.state = State::Standing;
                }
            }
            _ => {}
        }
        self.last_root = self.m.root().translation;
        self.m.step(dt, &self.steer);
        if self.m.action_finished() {
            match self.state {
                State::SittingDown => self.start_seated(),
                State::StandingUp => {
                    self.state = State::Standing;
                    if self.sit.is_none() {
                        self.seat = None;
                    }
                    let facing = self.m.root().rotation.yaw();
                    self.steer = Steer::Constant(ConstantSteering {
                        velocity: Vec3::ZERO,
                        facing,
                    });
                    if let Some(s) = self.after_standing.take() {
                        self.steer = s;
                        self.state = if matches!(self.steer, Steer::Path(_)) && self.sit.is_some() {
                            State::Approaching
                        } else {
                            State::Walking
                        };
                    }
                }
                _ => {}
            }
        }
        self.layers(dt);
    }

    /// The seated loop to follow a sit ending on frame `from`: the seated frame whose feet,
    /// its hips put where the sit's are, are nearest the sit's, with a long loop after it
    /// (a short loop shows its seam every time it restarts). Returns how far the feet would
    /// move (m) and the loop's frames.
    fn seated_after(&self, from: usize) -> Option<(f32, (usize, usize))> {
        let db = &self.db;
        let mut best: Option<(f32, (usize, usize))> = None;
        for min_loop in [(3.0 * db.fps) as usize, 2] {
            for c in &db.clips {
                if !self.clip_ok(c.tags, tags::SEATED) || c.tags & (tags::TALK | tags::GESTURE) != 0
                {
                    continue;
                }
                let mut f = c.start;
                while f + min_loop <= c.end {
                    let d = seat_distance(db, from, f);
                    if best.is_none_or(|b| d < b.0) {
                        best = Some((d, (f, c.end)));
                    }
                    f += 4;
                }
            }
            if best.is_some() {
                break;
            }
        }
        best
    }

    fn start_seated(&mut self) {
        let Some(seat) = self.seat else { return };
        let db = Arc::clone(&self.db);
        let hold = self.hold.take().or_else(|| {
            self.seated_after(self.sit.map_or(self.m.frame(), |a| a.end - 1))
                .map(|b| b.1)
        });
        let Some((start, end)) = hold else {
            self.state = State::Seated;
            return;
        };
        let facing = Quat::from_yaw(seat.facing);
        let root = Transform::new(
            seat.hips.flat() - facing.rotate(db.hips_pos[start].flat()),
            facing,
        );
        self.m.play(Action {
            start,
            end,
            looping: true,
            pin_root: true,
            root_target: Some(root),
            hips_height_target: Some(seat.hips.y),
        });
        self.state = State::Seated;
    }

    fn forward(&mut self) {
        let db = &self.db;
        let root = self.m.root();
        let hips = root.compose(Transform::new(self.m.hips_position(), self.rot[0]));
        self.world_pos[0] = hips.translation;
        self.world_rot[0] = hips.rotation;
        for i in 1..db.bone_count() {
            let p = db.skeleton.parents[i].unwrap_or(0);
            self.world_pos[i] = self.world_pos[p] + self.world_rot[p].rotate(self.local_pos[i]);
            self.world_rot[i] = self.world_rot[p] * self.rot[i];
        }
    }

    /// Turns bone `b` in the world by `q` (about its own position), keeping its children's
    /// local rotations, so they follow.
    fn turn_world(&mut self, b: usize, q: Quat) {
        let parent =
            self.db.skeleton.parents[b].map_or(self.m.root().rotation, |p| self.world_rot[p]);
        let w = (q * self.world_rot[b]).normalize();
        self.rot[b] = (parent.inverse() * w).normalize();
    }

    fn layers(&mut self, dt: f32) {
        self.rot.copy_from_slice(self.m.local_rotations());
        self.forward();
        let root = self.m.root();
        let yaw = root.rotation.yaw();
        let right = Vec3::new(-yaw.cos(), 0.0, yaw.sin());
        // Beats: a raised-cosine pulse a word, its stroke 50 ms before the word, the hands
        // taking turns; the forearm lifts, the hand flicks, the head nods a little.
        let mut kick = [0.0f32; 2];
        let mut nod = 0.0f32;
        let mut w = 0;
        for i in 0..self.beat_count {
            let t = self.beats[i];
            if self.clock - t > 0.4 {
                continue;
            }
            self.beats[w] = t;
            w += 1;
            let x = (self.clock - (t - 0.05)) / 0.22;
            if x.abs() < 1.0 {
                let e = 0.5 * (1.0 + (std::f32::consts::PI * x).cos()) * self.beat_strength;
                kick[i % 2] = kick[i % 2].max(e);
                nod = nod.max(e);
            }
        }
        self.beat_count = w;
        if self.state != State::Walking {
            for (side, k) in kick.iter().enumerate() {
                let Some(a) = self.arms[1 - side] else {
                    continue;
                };
                if *k <= 1e-4 {
                    continue;
                }
                let upper = (self.world_pos[a.fore] - self.world_pos[a.arm]).normalize_or_zero();
                let axis = upper.cross(Vec3::Y).normalize_or_zero();
                if axis.length_squared() > 0.5 {
                    self.turn_world(a.fore, Quat::from_axis_angle(axis, -0.32 * k));
                    self.forward();
                    let lower =
                        (self.world_pos[a.hand] - self.world_pos[a.fore]).normalize_or_zero();
                    let axis2 = lower.cross(Vec3::Y).normalize_or_zero();
                    if axis2.length_squared() > 0.5 {
                        self.turn_world(a.hand, Quat::from_axis_angle(axis2, -0.18 * k));
                    }
                }
            }
        }
        // Look-at: yaw and pitch from the body to the target, limited, eased, spread.
        self.look_weight = smooth_to(
            self.look_weight,
            if self.look.is_some() { 1.0 } else { 0.0 },
            0.15,
            dt,
        );
        let (mut ty, mut tp) = (0.0f32, 0.0f32);
        if let (Some(target), Some(head)) = (self.look, self.head) {
            let d = target - self.world_pos[head];
            ty = wrap_angle(d.x.atan2(d.z) - yaw).clamp(-1.2, 1.2);
            tp = d.y.atan2(d.flat().length().max(1e-3)).clamp(-0.5, 0.4);
        }
        self.look_angles.0 = smooth_to(self.look_angles.0, ty, 0.12, dt);
        self.look_angles.1 = smooth_to(self.look_angles.1, tp, 0.12, dt);
        let (ly, lp) = (
            self.look_angles.0 * self.look_weight,
            self.look_angles.1 * self.look_weight - nod * 0.06,
        );
        if ly.abs() > 1e-4 || lp.abs() > 1e-4 {
            let mut done = 0.0f32;
            for (bone, share) in [(self.spine2, 0.2f32), (self.neck, 0.35), (self.head, 0.45)] {
                let Some(b) = bone else { continue };
                let c = done + share;
                // This bone's share on top of what its parents already turned.
                let pitch_axis = Quat::from_yaw(c * ly).rotate(right);
                let total = Quat::from_axis_angle(pitch_axis, -c * lp) * Quat::from_yaw(c * ly);
                let pitch_axis0 = Quat::from_yaw(done * ly).rotate(right);
                let before =
                    Quat::from_axis_angle(pitch_axis0, -done * lp) * Quat::from_yaw(done * ly);
                self.turn_world(b, total * before.inverse());
                self.forward();
                done = c;
            }
        }
        // Hand IK.
        for side in 0..2 {
            self.hand_weights[side] = smooth_to(
                self.hand_weights[side],
                if self.hands[side].is_some() { 1.0 } else { 0.0 },
                0.15,
                dt,
            );
            let (Some(a), wgt) = (self.arms[side], self.hand_weights[side]) else {
                continue;
            };
            if wgt < 1e-3 {
                continue;
            }
            let target = self.hands[side].unwrap_or(self.world_pos[a.hand]);
            let sol = two_bone(
                self.world_pos[a.arm],
                self.world_pos[a.fore],
                self.world_pos[a.hand],
                target,
                -Vec3::Y,
                0.03,
                self.world_rot[a.arm],
                self.world_rot[a.fore],
            );
            let parent = self.world_rot[a.shoulder];
            let arm_l = (parent.inverse() * sol.root).normalize();
            let fore_l = (sol.root.inverse() * sol.mid).normalize();
            self.rot[a.arm] = self.rot[a.arm].slerp(arm_l, wgt);
            self.rot[a.fore] = self.rot[a.fore].slerp(fore_l, wgt);
            self.forward();
        }
    }

    /// Each driven bone's rotation after the layers (the hips' relative to the root, the
    /// others' relative to their parents).
    #[must_use]
    pub fn local_rotations(&self) -> &[Quat] {
        &self.rot
    }

    /// The hips' position relative to the root.
    #[must_use]
    pub fn hips_position(&self) -> Vec3 {
        self.m.hips_position()
    }

    /// The root on the ground.
    #[must_use]
    pub fn root(&self) -> Transform {
        self.m.root()
    }

    /// World positions of the driven bones after the layers.
    #[must_use]
    pub fn world_positions(&self) -> &[Vec3] {
        &self.world_pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::BuildConfig;
    use crate::testing::{humanoid_skeleton, synthetic_idle, synthetic_sit, synthetic_walk};

    fn db() -> Arc<Database> {
        let target = humanoid_skeleton(1.0);
        let mut clips = vec![
            synthetic_walk(&target, 600, 1.0),
            synthetic_walk(&target, 600, 0.7),
            synthetic_idle(&target, 300),
        ];
        clips.extend(synthetic_sit(&target));
        Arc::new(Database::build(&target, &clips, &BuildConfig::default()).unwrap())
    }

    #[test]
    fn walks_sits_on_a_seat_and_stands_again() {
        let mut c = Character::new(db(), Config::default(), Vec3::ZERO, 0.0);
        // Facing +Z, its back to a seat ahead of where it stops: no turn on the spot, which
        // the synthetic clips cannot do (real data turns in place).
        let seat = Seat {
            hips: Vec3::new(0.0, 0.5, 3.0),
            facing: 0.0,
        };
        c.sit(seat);
        assert_eq!(c.state(), State::Approaching);
        let mut seen_seated = false;
        for i in 0..(60 * 20) {
            c.step(1.0 / 60.0);
            if std::env::var_os("CHAR_DEBUG").is_some() && i % 60 == 0 {
                eprintln!("t {} root {:?} approach {:?} state {:?} frame {} clip {:?} searches {} trans {} sim {:?}", i / 60, c.root().translation, c.approach.translation, c.state(), c.m.frame(), c.db.clip_of(c.m.frame()).map(|k| (&k.name, k.tags)), c.m.stats().searches, c.m.stats().transitions, c.m.simulation().translation);
            }
            if c.state() == State::Seated {
                seen_seated = true;
                break;
            }
        }
        assert!(seen_seated, "never sat: {:?}", c.state());
        for _ in 0..120 {
            c.step(1.0 / 60.0);
        }
        let hips = c.world_positions()[0];
        assert!(
            (hips - seat.hips).length() < 0.12,
            "hips {hips:?} against {:?}",
            seat.hips
        );
        c.go_to(&[c.root().translation, Vec3::new(2.0, 0.0, 3.0)], 0.8, None);
        assert_eq!(c.state(), State::StandingUp);
        for i in 0..(60 * 12) {
            c.step(1.0 / 60.0);
            if std::env::var_os("CHAR_DEBUG").is_some() && i % 30 == 0 {
                eprintln!(
                    "u {} root {:?} state {:?} action {:?} clip {:?}",
                    i,
                    c.root().translation,
                    c.state(),
                    c.m.action().map(|a| a.1),
                    c.db.clip_of(c.m.frame()).map(|k| &k.name)
                );
            }
        }
        assert!(
            matches!(c.state(), State::Standing | State::Walking),
            "{:?}",
            c.state()
        );
        assert!(
            (c.root().translation - Vec3::new(2.0, 0.0, 3.0)).length() < 0.6,
            "{:?}",
            c.root().translation
        );
        assert!(c.world_positions().iter().all(|p| p.is_finite()));
    }

    #[test]
    fn look_at_turns_the_head_and_beats_move_a_hand() {
        let mut c = Character::new(db(), Config::default(), Vec3::ZERO, 0.0);
        for _ in 0..60 {
            c.step(1.0 / 60.0);
        }
        let head = find_bone(&c.db.skeleton, "Head").unwrap();
        let before = c.world_positions()[head];
        c.look_at(Some(Vec3::new(3.0, 1.6, 0.5)));
        for _ in 0..60 {
            c.step(1.0 / 60.0);
        }
        let fwd = c.world_rot[head].rotate(Vec3::Z);
        assert!(fwd.x > 0.3, "the head did not turn: {fwd:?}");
        assert!((c.world_positions()[head] - before).length() < 0.15);
        let hand = find_bone(&c.db.skeleton, "RightHand").unwrap();
        let mut moved = 0.0f32;
        c.gesture(&[0.3], 1.0);
        let start = c.world_positions()[hand];
        for _ in 0..40 {
            c.step(1.0 / 60.0);
            moved = moved.max((c.world_positions()[hand] - start).length());
        }
        assert!(moved > 0.03, "the hand moved {moved}");
    }
}
