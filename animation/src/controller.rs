//! The motion-matching runtime: [`MotionMatcher::step`] advances one character by `dt`.
//!
//! Each step:
//!
//! 1. **Simulation.** A critically damped spring moves a simulated root towards the
//!    steering's desired velocity and facing, and predicts where it will be ⅓, ⅔ and 1 s
//!    ahead (evaluating the steering along the way, so a path's turn is anticipated).
//! 2. **Search**, every [`Config::search_interval`] or sooner when the desire changes
//!    sharply or the clip ends: the query is the current frame's pose features with the
//!    predicted trajectory, and the database's nearest frame wins if it beats carrying on
//!    by [`Config::transition_cost`].
//! 3. **Playback.** The current database frame advances by `dt` (any frame rate: poses
//!    are interpolated between the database's frames), and its root motion moves the
//!    character.
//! 4. **Transition by inertialization.** The pose and its velocity stay continuous: each
//!    bone's difference from the new frame decays like a released spring.
//! 5. **Root.** The character's root follows the animation's own root motion, nudged
//!    towards the simulation only as fast as it is moving already, and never more than
//!    [`Config::clamp_distance`] away.
//! 6. **Feet.** Where the database marks a heel or ball contact, that point is held where
//!    it touched down; two-bone IK keeps the ankle there, and releasing blends back out.
//!
//! The step allocates nothing: every buffer is sized when the matcher is made.

use std::sync::Arc;

use crate::database::{Database, FEATURES, HORIZONS, LEFT_BALL, LEFT_HEEL, RIGHT_BALL, RIGHT_HEEL};
use crate::ik::two_bone;
use crate::math::{wrap_angle, Quat, Transform, Vec3};
use crate::path::Steering;
use crate::search::{search, Exclusions, Match, SearchStats};
use crate::spring::{angular_velocity, InertialQuat, InertialVec3, RotationSpring, VelocitySpring};

/// Tuning of the runtime. The defaults suit a walking humanoid at 30-240 Hz.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    /// Seconds between searches.
    pub search_interval: f32,
    /// Half-life of transition blends (s).
    pub blend_halflife: f32,
    /// Half-lives of the simulation's velocity and facing springs (s).
    pub velocity_halflife: f32,
    pub facing_halflife: f32,
    /// Half-lives of the root's pull towards the simulation (s).
    pub adjust_position_halflife: f32,
    pub adjust_facing_halflife: f32,
    /// The pull may move the root at most this fraction of its own speed.
    pub adjust_velocity_ratio: f32,
    /// The root and the simulation never stray further apart than this (m, rad).
    pub clamp_distance: f32,
    pub clamp_angle: f32,
    /// At that limit, hold the simulation back to the character (true) rather than drag
    /// the character to the simulation (false, which slides planted feet).
    pub synchronize: bool,
    /// Hold planted feet with IK.
    pub foot_lock: bool,
    /// A held point further than this from the animated foot lets go (m).
    pub unlock_radius: f32,
    /// Half-life of the blend back after letting go (s).
    pub unlock_halflife: f32,
    /// The legs' soft reach: the last fraction of a leg's length approached slowly.
    pub soft_reach: f32,
    /// Frames either side of the current one the search skips.
    pub skip_surrounding: usize,
    /// Frames before a clip's end the search never lands on.
    pub end_margin: usize,
    /// A change of desired velocity bigger than this (m/s) searches at once.
    pub force_search_velocity: f32,
    /// A change of desired facing bigger than this (rad) searches at once.
    pub force_search_facing: f32,
    /// A transition must beat carrying on by at least this much feature cost (squared
    /// normalised distance), so near-equal frames do not trade places.
    pub transition_cost: f32,
    /// Keep the root's velocity continuous across a transition (true) or let the new
    /// clip's root motion take over at once (false).
    pub blend_root_velocity: bool,
    /// Half-life (s) of the simulation settling onto the character once both are still (0
    /// snaps it there: any lag lets the character hunt back and forth after the simulation).
    pub settle_halflife: f32,
    /// Standing still and within this angle (rad) of the facing wanted, keep the facing.
    pub settle_facing: f32,
    /// Never let the simulation trail the character along its travel (true): it may lead
    /// a character that lags, which is what starts a walk, but one that trails asks the
    /// character to stop and then lunge after it. False leaves the simulation alone, as
    /// Holden's demo does.
    pub simulate_from_character: bool,
    /// Play a walking clip up to this fraction faster or slower to match the speed
    /// wanted (0 plays everything as captured). Contacts stay planted; cadence changes.
    pub time_warp: f32,
}

impl Config {
    /// Sets one field by name from text, for tuning tools (`clamp_distance=0.25`).
    ///
    /// # Errors
    /// An unknown name or a value that does not parse.
    pub fn set(&mut self, name: &str, value: &str) -> Result<(), String> {
        let f = || {
            value
                .parse::<f32>()
                .map_err(|_| format!("{name}: {value:?} is not a number"))
        };
        let u = || {
            value
                .parse::<usize>()
                .map_err(|_| format!("{name}: {value:?} is not a whole number"))
        };
        let b = || {
            value
                .parse::<bool>()
                .map_err(|_| format!("{name}: {value:?} is not true or false"))
        };
        match name {
            "search_interval" => self.search_interval = f()?,
            "blend_halflife" => self.blend_halflife = f()?,
            "velocity_halflife" => self.velocity_halflife = f()?,
            "facing_halflife" => self.facing_halflife = f()?,
            "adjust_position_halflife" => self.adjust_position_halflife = f()?,
            "adjust_facing_halflife" => self.adjust_facing_halflife = f()?,
            "adjust_velocity_ratio" => self.adjust_velocity_ratio = f()?,
            "clamp_distance" => self.clamp_distance = f()?,
            "clamp_angle" => self.clamp_angle = f()?,
            "synchronize" => self.synchronize = b()?,
            "foot_lock" => self.foot_lock = b()?,
            "unlock_radius" => self.unlock_radius = f()?,
            "unlock_halflife" => self.unlock_halflife = f()?,
            "soft_reach" => self.soft_reach = f()?,
            "skip_surrounding" => self.skip_surrounding = u()?,
            "end_margin" => self.end_margin = u()?,
            "force_search_velocity" => self.force_search_velocity = f()?,
            "force_search_facing" => self.force_search_facing = f()?,
            "transition_cost" => self.transition_cost = f()?,
            "blend_root_velocity" => self.blend_root_velocity = b()?,
            "settle_halflife" => self.settle_halflife = f()?,
            "settle_facing" => self.settle_facing = f()?,
            "simulate_from_character" => self.simulate_from_character = b()?,
            "time_warp" => self.time_warp = f()?,
            _ => return Err(format!("no setting named {name:?}")),
        }
        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            search_interval: 0.1,
            blend_halflife: 0.1,
            velocity_halflife: 0.3,
            facing_halflife: 0.3,
            adjust_position_halflife: 0.15,
            adjust_facing_halflife: 0.2,
            adjust_velocity_ratio: 0.3,
            clamp_distance: 0.2,
            clamp_angle: 0.6,
            synchronize: true,
            foot_lock: true,
            unlock_radius: 0.2,
            unlock_halflife: 0.1,
            soft_reach: 0.03,
            skip_surrounding: 20,
            end_margin: 20,
            force_search_velocity: 0.25,
            force_search_facing: 0.35,
            transition_cost: 0.5,
            blend_root_velocity: true,
            settle_halflife: 0.0,
            settle_facing: 0.45,
            simulate_from_character: true,
            time_warp: 0.0,
        }
    }
}

/// Counters of what the matcher has done.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stats {
    pub steps: u64,
    pub searches: u64,
    pub transitions: u64,
    /// Frames whose full distance the last search computed.
    pub last_frames_tested: usize,
    /// The last search's best cost.
    pub last_cost: f32,
    /// Feet let go because the animation moved too far from the held point.
    pub unlocks_by_radius: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Point {
    Heel,
    Ball,
}

#[derive(Debug, Clone, Copy)]
struct FootLock {
    held: Option<(Point, Vec3)>,
    /// Let go at the unlock radius: not held again until this contact ends.
    released: Option<Point>,
    /// The correction applied to the ankle last step, and the step before.
    correction: Vec3,
    previous_correction: Vec3,
    release: InertialVec3,
}

impl Default for FootLock {
    fn default() -> Self {
        Self {
            held: None,
            released: None,
            correction: Vec3::ZERO,
            previous_correction: Vec3::ZERO,
            release: InertialVec3::default(),
        }
    }
}

/// One character driven by motion matching.
pub struct MotionMatcher {
    db: Arc<Database>,
    config: Config,
    frame: usize,
    frac: f32,
    search_timer: f32,
    /// Playback reached its clip's end: search on the next step.
    at_clip_end: bool,
    /// Playback rate (1 = as captured).
    rate: f32,
    last_desired: (Vec3, f32),
    // Inertialization.
    bone_blend: Vec<InertialQuat>,
    hips_blend: InertialVec3,
    root_vel_blend: InertialVec3,
    /// The root's yaw rate offset, in `offset.x` (rad/s).
    yaw_rate_blend: InertialVec3,
    // Maps the database's clip space into the world.
    anchor: Transform,
    root: Transform,
    root_velocity: Vec3,
    root_yaw_rate: f32,
    // Simulation.
    sim_pos: VelocitySpring,
    sim_rot: RotationSpring,
    trajectory: [(Vec3, Quat); 3],
    // Pose buffers.
    anim_rot: Vec<Quat>,
    out_rot: Vec<Quat>,
    out_hips: Vec3,
    local_pos: Vec<Vec3>,
    world_rot: Vec<Quat>,
    world_pos: Vec<Vec3>,
    feet: [FootLock; 2],
    query: [f32; FEATURES],
    stats: Stats,
}

impl MotionMatcher {
    /// A matcher standing at `position` facing `facing` (radians about +Y), playing the
    /// database frame that best suits standing still.
    #[must_use]
    pub fn new(db: Arc<Database>, config: Config, position: Vec3, facing: f32) -> Self {
        let bones = db.bone_count();
        let rot = Quat::from_yaw(facing);
        let local_pos: Vec<Vec3> = db.skeleton.rest.iter().map(|t| t.translation).collect();
        let mut m = Self {
            config,
            frame: 0,
            frac: 0.0,
            search_timer: 0.0,
            at_clip_end: false,
            rate: 1.0,
            last_desired: (Vec3::ZERO, facing),
            bone_blend: vec![
                InertialQuat {
                    offset: Quat::IDENTITY,
                    offset_vel: Vec3::ZERO
                };
                bones
            ],
            hips_blend: InertialVec3::default(),
            root_vel_blend: InertialVec3::default(),
            yaw_rate_blend: InertialVec3::default(),
            anchor: Transform::IDENTITY,
            root: Transform::new(position.flat(), rot),
            root_velocity: Vec3::ZERO,
            root_yaw_rate: 0.0,
            sim_pos: VelocitySpring {
                position: position.flat(),
                ..VelocitySpring::default()
            },
            sim_rot: RotationSpring {
                rotation: rot,
                angular_velocity: Vec3::ZERO,
            },
            trajectory: [(position.flat(), rot); 3],
            anim_rot: vec![Quat::IDENTITY; bones],
            out_rot: vec![Quat::IDENTITY; bones],
            out_hips: Vec3::ZERO,
            local_pos,
            world_rot: vec![Quat::IDENTITY; bones],
            world_pos: vec![Vec3::ZERO; bones],
            feet: [FootLock::default(); 2],
            query: [0.0; FEATURES],
            stats: Stats::default(),
            db,
        };
        // Start on the frame whose trajectory best matches standing still here (there is
        // no pose yet to match, so the pose features are left out).
        m.build_query();
        let mut best = (f32::INFINITY, 0usize);
        for f in 0..m.db.frame_count() {
            if f + m.config.end_margin >= m.clip_end(f) {
                continue;
            }
            let feat = m.db.feature(f);
            let cost: f32 = (15..FEATURES).map(|k| (m.query[k] - feat[k]).powi(2)).sum();
            if cost < best.0 {
                best = (cost, f);
            }
        }
        m.frame = best.1;
        m.anchor = m.root.compose(m.db_root(m.frame, 0.0).inverse());
        m.sample(m.frame, 0.0);
        m.out_rot.copy_from_slice(&m.anim_rot);
        m.out_hips = m.db.hips_pos[m.frame];
        m.pose_world();
        m
    }

    /// The settings in use.
    #[must_use]
    pub fn config(&self) -> Config {
        self.config
    }

    /// Changes the settings; takes effect from the next step.
    pub fn set_config(&mut self, config: Config) {
        self.config = config;
    }

    /// The database this matcher plays.
    #[must_use]
    pub fn database(&self) -> &Database {
        &self.db
    }

    #[must_use]
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// The database frame playing now.
    #[must_use]
    pub fn frame(&self) -> usize {
        self.frame
    }

    /// The character's root on the ground (position and facing).
    #[must_use]
    pub fn root(&self) -> Transform {
        self.root
    }

    /// The simulated root the character is steered by.
    #[must_use]
    pub fn simulation(&self) -> Transform {
        Transform::new(self.sim_pos.position, self.sim_rot.rotation)
    }

    /// The predicted trajectory (world positions and facings ⅓, ⅔ and 1 s ahead).
    #[must_use]
    pub fn trajectory(&self) -> &[(Vec3, Quat); 3] {
        &self.trajectory
    }

    /// Each driven bone's rotation: the hips' relative to the root, the others' relative to
    /// their parents (see [`Database::skeleton`]).
    #[must_use]
    pub fn local_rotations(&self) -> &[Quat] {
        &self.out_rot
    }

    /// The hips' position relative to the root.
    #[must_use]
    pub fn hips_position(&self) -> Vec3 {
        self.out_hips
    }

    /// World positions of the driven bones after the last step.
    #[must_use]
    pub fn world_positions(&self) -> &[Vec3] {
        &self.world_pos
    }

    /// World rotations of the driven bones after the last step.
    #[must_use]
    pub fn world_rotations(&self) -> &[Quat] {
        &self.world_rot
    }

    /// Whether each foot (left, right) is held planted now.
    #[must_use]
    pub fn feet_held(&self) -> [bool; 2] {
        [self.feet[0].held.is_some(), self.feet[1].held.is_some()]
    }

    /// The database contact bits of the frame playing now.
    #[must_use]
    pub fn contacts(&self) -> u8 {
        self.db.contacts[self.frame]
    }

    /// Places the character (and its simulation) at once, as a cut.
    pub fn teleport(&mut self, position: Vec3, facing: f32) {
        let rot = Quat::from_yaw(facing);
        let new_root = Transform::new(position.flat(), rot);
        let delta = new_root.compose(self.root.inverse());
        self.anchor = delta.compose(self.anchor);
        self.root = new_root;
        self.sim_pos = VelocitySpring {
            position: position.flat(),
            ..VelocitySpring::default()
        };
        self.sim_rot = RotationSpring {
            rotation: rot,
            angular_velocity: Vec3::ZERO,
        };
        for f in &mut self.feet {
            *f = FootLock::default();
        }
    }

    fn db_root(&self, frame: usize, frac: f32) -> Transform {
        let db = &self.db;
        let next = (frame + 1).min(db.frame_count() - 1);
        Transform::new(
            db.root_pos[frame].lerp(db.root_pos[next], frac),
            db.root_rot[frame].slerp(db.root_rot[next], frac),
        )
    }

    fn clip_end(&self, frame: usize) -> usize {
        self.db.bounds.clip_end[frame] as usize
    }

    fn sample(&mut self, frame: usize, frac: f32) {
        let db = &self.db;
        let b = db.bone_count();
        let next = (frame + 1).min(self.clip_end(frame) - 1);
        let (r0, r1) = (
            &db.rotations[frame * b..(frame + 1) * b],
            &db.rotations[next * b..(next + 1) * b],
        );
        for i in 0..b {
            self.anim_rot[i] = r0[i].slerp(r1[i], frac);
        }
    }

    fn predict<S: Steering>(&mut self, steering: &S) {
        let mut p = self.sim_pos;
        let mut r = self.sim_rot;
        let step: f32 = 1.0 / 20.0;
        let mut t = 0.0f32;
        for (k, h) in HORIZONS.iter().enumerate() {
            let until = *h as f32 / self.db.fps;
            while t < until - 1e-6 {
                let dt = step.min(until - t);
                let (v, f) = steering.desired(p.position);
                p.update(v.flat(), self.config.velocity_halflife, dt);
                r.update(Quat::from_yaw(f), self.config.facing_halflife, dt);
                t += dt;
            }
            self.trajectory[k] = (p.position, r.rotation);
        }
    }

    fn build_query(&mut self) {
        let db = &self.db;
        self.query[..15].copy_from_slice(&db.feature(self.frame)[..15]);
        for (k, (p, r)) in self.trajectory.iter().enumerate() {
            let lp = self.root.inverse_apply(*p);
            let d = self.root.rotation.inverse_rotate(r.rotate(Vec3::Z));
            let (i, j) = (15 + 2 * k, 21 + 2 * k);
            self.query[i] = (lp.x - db.feature_mean[i]) / db.feature_scale[i];
            self.query[i + 1] = (lp.z - db.feature_mean[i + 1]) / db.feature_scale[i + 1];
            self.query[j] = (d.x - db.feature_mean[j]) / db.feature_scale[j];
            self.query[j + 1] = (d.z - db.feature_mean[j + 1]) / db.feature_scale[j + 1];
        }
    }

    fn cost_of(&self, frame: usize) -> f32 {
        let f = self.db.feature(frame);
        (0..FEATURES).map(|k| (self.query[k] - f[k]).powi(2)).sum()
    }

    fn transition(&mut self, to: usize) {
        let db = Arc::clone(&self.db);
        let b = db.bone_count();
        let dt = 1.0 / db.fps;
        let (f0, f1) = (
            self.frame,
            (self.frame + 1).min(self.clip_end(self.frame) - 1),
        );
        let (t0, t1) = (to, (to + 1).min(self.clip_end(to) - 1));
        for i in 0..b {
            let src = db.rotations[f0 * b + i].slerp(db.rotations[f1 * b + i], self.frac);
            let src_w = angular_velocity(db.rotations[f0 * b + i], db.rotations[f1 * b + i], dt);
            let dst = db.rotations[t0 * b + i];
            let dst_w = angular_velocity(db.rotations[t0 * b + i], db.rotations[t1 * b + i], dt);
            self.bone_blend[i].transition(src, src_w, dst, dst_w);
        }
        let src = db.hips_pos[f0].lerp(db.hips_pos[f1], self.frac);
        let src_v = (db.hips_pos[f1] - db.hips_pos[f0]) / dt;
        let dst_v = (db.hips_pos[t1] - db.hips_pos[t0]) / dt;
        self.hips_blend
            .transition(src, src_v, db.hips_pos[t0], dst_v);
        // Root: continue from where the character is; keep its velocity continuous.
        let new_db_root = self.db_root(t0, 0.0);
        let new_anchor = self.root.compose(new_db_root.inverse());
        let v_new = new_anchor
            .rotation
            .rotate((db.root_pos[t1] - db.root_pos[t0]) / dt);
        if self.config.blend_root_velocity {
            self.root_vel_blend
                .transition(self.root_velocity, Vec3::ZERO, v_new, Vec3::ZERO);
        }
        let yaw_new = wrap_angle(db.root_rot[t1].yaw() - db.root_rot[t0].yaw()) / dt;
        self.yaw_rate_blend.transition(
            Vec3::new(self.root_yaw_rate, 0.0, 0.0),
            Vec3::ZERO,
            Vec3::new(yaw_new, 0.0, 0.0),
            Vec3::ZERO,
        );
        self.anchor = new_anchor;
        self.frame = to;
        self.frac = 0.0;
        self.stats.transitions += 1;
    }

    /// Advances the character by `dt` seconds towards what `steering` wants.
    pub fn step<S: Steering>(&mut self, dt: f32, steering: &S) {
        let dt = dt.clamp(1e-4, 0.1);
        let cfg = self.config;
        self.stats.steps += 1;

        // 1. Simulation and prediction.
        if cfg.simulate_from_character {
            // The simulation may lead a lagging character (that lead is what starts a walk)
            // but never trails one: a simulation behind the character along its travel asks
            // it to stop, it stops, the simulation passes, and it lunges after it.
            let d = (self.sim_pos.position - self.root.translation).flat();
            let v = self.sim_pos.velocity.flat();
            if v.length() > 0.05 {
                let dir = v.normalize_or_zero();
                let along = d.dot(dir);
                if along < 0.0 {
                    self.sim_pos.position -= dir * along;
                }
            }
        }
        let (desired_v, mut desired_f) = steering.desired(self.sim_pos.position);
        let resting = desired_v.flat().length() < 0.05 && self.sim_pos.velocity.length() < 0.05;
        // Standing, and already facing about the way wanted: keep the facing, rather than
        // shuffle round the last few degrees.
        let root_yaw = self.root.rotation.yaw();
        if resting && wrap_angle(desired_f - root_yaw).abs() < cfg.settle_facing {
            desired_f = root_yaw;
        }
        self.sim_pos
            .update(desired_v.flat(), cfg.velocity_halflife, dt);
        // At rest, with nobody wanting to move, the simulation settles onto the character:
        // a few centimetres short of a goal is a person standing, not a cue to shuffle.
        if resting {
            let k = 1.0 - (-std::f32::consts::LN_2 * dt / cfg.settle_halflife).exp();
            self.sim_pos.position += (self.root.translation - self.sim_pos.position).flat() * k;
        }
        self.sim_rot
            .update(Quat::from_yaw(desired_f), cfg.facing_halflife, dt);
        self.predict(steering);
        let force = (desired_v - self.last_desired.0).length() > cfg.force_search_velocity
            || wrap_angle(desired_f - self.last_desired.1).abs() > cfg.force_search_facing;

        // 2. Search, on the frame playing now (and at once if it reached its clip's end).
        self.search_timer -= dt;
        let end_of_clip = std::mem::take(&mut self.at_clip_end);
        if self.search_timer <= 0.0 || force || end_of_clip {
            self.search_timer = cfg.search_interval;
            if force {
                self.last_desired = (desired_v, desired_f);
            }
            self.build_query();
            let current = if end_of_clip {
                None
            } else {
                Some(Match {
                    frame: self.frame,
                    cost: self.cost_of(self.frame) - cfg.transition_cost,
                })
            };
            let clip_start = self.db.clip_of(self.frame).map_or(0, |c| c.start);
            let skip = (
                self.frame
                    .saturating_sub(cfg.skip_surrounding)
                    .max(clip_start),
                (self.frame + cfg.skip_surrounding).min(self.clip_end(self.frame)),
            );
            let ex = Exclusions {
                end_margin: cfg.end_margin,
                skip: if end_of_clip { (0, 0) } else { skip },
            };
            let mut st = SearchStats::default();
            let found = search(&self.db, &self.query, current, ex, &mut st);
            self.stats.searches += 1;
            self.stats.last_frames_tested = st.frames_tested;
            if let Some(best) = found {
                self.stats.last_cost = best.cost;
                if current.is_none_or(|c| best.frame != c.frame) {
                    self.transition(best.frame);
                }
            }
        }
        // 3. Playback, of whichever clip is playing now, a little faster or slower (time
        // warp) when the clip walks at another speed than the one wanted.
        let prev_db_root = self.db_root(self.frame, self.frac);
        let target_rate = if cfg.time_warp > 0.0 {
            let next = (self.frame + 1).min(self.clip_end(self.frame) - 1);
            let clip_speed = (self.db.root_pos[next] - self.db.root_pos[self.frame])
                .flat()
                .length()
                * self.db.fps;
            let want = self.sim_pos.velocity.flat().length();
            if clip_speed > 0.3 && want > 0.3 {
                (want / clip_speed).clamp(1.0 - cfg.time_warp, 1.0 + cfg.time_warp)
            } else {
                1.0
            }
        } else {
            1.0
        };
        self.rate +=
            (target_rate - self.rate) * (1.0 - (-std::f32::consts::LN_2 * dt / 0.25).exp());
        self.frac += dt * self.db.fps * self.rate;
        while self.frac >= 1.0 {
            self.frac -= 1.0;
            if self.frame + 2 >= self.clip_end(self.frame) {
                self.frac = 0.0;
                self.at_clip_end = true;
                break;
            }
            self.frame += 1;
        }
        // Root motion over this step, carried into the world by the anchor.
        let db_root_now = self.db_root(self.frame, self.frac);
        let delta_pos = self
            .anchor
            .rotation
            .rotate(db_root_now.translation - prev_db_root.translation);
        let delta_yaw = wrap_angle(db_root_now.rotation.yaw() - prev_db_root.rotation.yaw());

        if !force && (desired_v - self.last_desired.0).length() > 0.05 {
            // Track slow drift of the desire without forcing.
            self.last_desired = (desired_v, desired_f);
        }

        // 4. Pose with inertialization.
        self.sample(self.frame, self.frac);
        let b = self.db.bone_count();
        for i in 0..b {
            self.out_rot[i] = self.bone_blend[i].update(self.anim_rot[i], cfg.blend_halflife, dt);
        }
        let next = (self.frame + 1).min(self.clip_end(self.frame) - 1);
        let hips_anim = self.db.hips_pos[self.frame].lerp(self.db.hips_pos[next], self.frac);
        self.out_hips = self.hips_blend.update(hips_anim, cfg.blend_halflife, dt);

        // 5. Root: animation root motion, blended velocity, nudged towards the simulation.
        let anim_vel = delta_pos / dt;
        let vel = self.root_vel_blend.update(anim_vel, cfg.blend_halflife, dt);
        let yaw_rate = self
            .yaw_rate_blend
            .update(Vec3::new(delta_yaw / dt, 0.0, 0.0), cfg.blend_halflife, dt)
            .x;
        let mut pos = self.root.translation + vel * dt;
        let mut yaw = self.root.rotation.yaw() + yaw_rate * dt;
        // Adjustment, limited by the root's own speed.
        let to_sim = (self.sim_pos.position - pos).flat();
        let pull =
            to_sim * (1.0 - (-std::f32::consts::LN_2 * dt / cfg.adjust_position_halflife).exp());
        let max_pull = cfg.adjust_velocity_ratio * vel.length() * dt;
        pos += if pull.length() > max_pull {
            pull.normalize_or_zero() * max_pull
        } else {
            pull
        };
        let sim_yaw = self.sim_rot.rotation.yaw();
        let yaw_err = wrap_angle(sim_yaw - yaw);
        let turn =
            yaw_err * (1.0 - (-std::f32::consts::LN_2 * dt / cfg.adjust_facing_halflife).exp());
        let max_turn = cfg.adjust_velocity_ratio * yaw_rate.abs().max(vel.length() * 0.5) * dt;
        yaw += turn.clamp(-max_turn, max_turn);
        // Keep the two within reach of each other. Synchronised, the simulation waits for
        // the character (it is pulled back, keeping its velocity), so the body is never
        // dragged over planted feet; otherwise the character is pulled to the simulation.
        let off = (pos - self.sim_pos.position).flat();
        if off.length() > cfg.clamp_distance {
            if cfg.synchronize {
                self.sim_pos.position = pos.flat() - off.normalize_or_zero() * cfg.clamp_distance;
            } else {
                pos = self.sim_pos.position + off.normalize_or_zero() * cfg.clamp_distance;
            }
        }
        let yaw_off_sim = wrap_angle(yaw - sim_yaw);
        if yaw_off_sim.abs() > cfg.clamp_angle {
            if cfg.synchronize {
                self.sim_rot.rotation =
                    Quat::from_yaw(yaw - yaw_off_sim.signum() * cfg.clamp_angle);
            } else {
                yaw = sim_yaw + yaw_off_sim.signum() * cfg.clamp_angle;
            }
        }
        let new_root = Transform::new(pos.flat(), Quat::from_yaw(yaw));
        self.root_velocity = (new_root.translation - self.root.translation) / dt;
        self.root_yaw_rate = wrap_angle(yaw - self.root.rotation.yaw()) / dt;
        // Keep the anchor consistent with where the root now is.
        self.anchor = new_root.compose(db_root_now.inverse());
        self.root = new_root;

        // 6. World pose and feet.
        self.pose_world();
        if cfg.foot_lock {
            self.lock_feet(dt);
        }
    }

    fn pose_world(&mut self) {
        let db = &self.db;
        let hips = self
            .root
            .compose(Transform::new(self.out_hips, self.out_rot[0]));
        self.world_pos[0] = hips.translation;
        self.world_rot[0] = hips.rotation;
        for i in 1..db.bone_count() {
            let p = db.skeleton.parents[i].unwrap_or(0);
            self.world_pos[i] = self.world_pos[p] + self.world_rot[p].rotate(self.local_pos[i]);
            self.world_rot[i] = self.world_rot[p] * self.out_rot[i];
        }
    }

    fn refresh_leg(&mut self, up: usize) {
        // Recompute world transforms below the hip joint after its rotations changed.
        let db = &self.db;
        let n = db.bone_count();
        for i in up..n {
            let Some(p) = db.skeleton.parents[i] else {
                continue;
            };
            if i == up || self.is_below(i, up) {
                self.world_pos[i] = self.world_pos[p] + self.world_rot[p].rotate(self.local_pos[i]);
                self.world_rot[i] = self.world_rot[p] * self.out_rot[i];
            }
        }
    }

    fn is_below(&self, bone: usize, ancestor: usize) -> bool {
        let mut b = self.db.skeleton.parents[bone];
        while let Some(p) = b {
            if p == ancestor {
                return true;
            }
            b = self.db.skeleton.parents[p];
        }
        false
    }

    fn lock_feet(&mut self, dt: f32) {
        let cfg = self.config;
        let contacts = self.db.contacts[self.frame];
        let heights = self.db.foot_heights;
        for side in 0..2 {
            let leg = self.db.humanoid.legs[side];
            let (heel_bit, ball_bit) = if side == 0 {
                (LEFT_HEEL, LEFT_BALL)
            } else {
                (RIGHT_HEEL, RIGHT_BALL)
            };
            let ankle_in = self.world_pos[leg.ankle];
            let toe_in = self.world_pos[leg.toe];
            let want = if contacts & ball_bit != 0 {
                Some(Point::Ball)
            } else if contacts & heel_bit != 0 {
                Some(Point::Heel)
            } else {
                None
            };
            let foot = &mut self.feet[side];
            if foot.released.is_some() && foot.released != want {
                foot.released = None;
            }
            let want = if foot.released.is_some() { None } else { want };
            let point_in = |p: Point| if p == Point::Heel { ankle_in } else { toe_in };
            let changed = match (foot.held, want) {
                (Some((p, _)), Some(w)) => p != w,
                (None, None) => false,
                _ => true,
            };
            if changed {
                match want {
                    Some(w) => {
                        let mut at = point_in(w) + foot.correction;
                        at.y = if w == Point::Heel {
                            heights[0]
                        } else {
                            heights[1]
                        };
                        foot.held = Some((w, at));
                    }
                    None => {
                        foot.release.offset = foot.correction;
                        foot.release.offset_vel = (foot.correction - foot.previous_correction) / dt;
                        foot.held = None;
                    }
                }
            }
            let correction = match foot.held {
                Some((p, at)) => {
                    let c = at - point_in(p);
                    if c.length() > cfg.unlock_radius {
                        foot.release.offset = foot.correction;
                        foot.release.offset_vel = Vec3::ZERO;
                        foot.released = Some(p);
                        foot.held = None;
                        self.stats.unlocks_by_radius += 1;
                        foot.release.update(Vec3::ZERO, cfg.unlock_halflife, dt)
                    } else {
                        c
                    }
                }
                None => foot.release.update(Vec3::ZERO, cfg.unlock_halflife, dt),
            };
            foot.previous_correction = foot.correction;
            foot.correction = correction;
            if correction.length_squared() < 1e-10 {
                continue;
            }
            // Two-bone IK puts the ankle at its corrected place; the foot keeps its turn.
            let target = ankle_in + correction;
            let ankle_world_rot = self.world_rot[leg.ankle];
            let toe_world_rot = self.world_rot[leg.toe];
            let pole = self.root.rotation.rotate(Vec3::Z);
            let sol = two_bone(
                self.world_pos[leg.up],
                self.world_pos[leg.knee],
                ankle_in,
                target,
                pole,
                cfg.soft_reach,
                self.world_rot[leg.up],
                self.world_rot[leg.knee],
            );
            let parent = self.db.skeleton.parents[leg.up].unwrap_or(0);
            self.out_rot[leg.up] = (self.world_rot[parent].inverse() * sol.root).normalize();
            self.out_rot[leg.knee] = (sol.root.inverse() * sol.mid).normalize();
            self.out_rot[leg.ankle] = (sol.mid.inverse() * ankle_world_rot).normalize();
            self.out_rot[leg.toe] = (ankle_world_rot.inverse() * toe_world_rot).normalize();
            self.refresh_leg(leg.up);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::BuildConfig;
    use crate::path::{ConstantSteering, PathSteering};
    use crate::testing::{humanoid_skeleton, synthetic_idle, synthetic_walk};

    fn db() -> Arc<Database> {
        let target = humanoid_skeleton(1.0);
        let clips = [
            synthetic_walk(&target, 600, 1.0),
            synthetic_walk(&target, 600, 0.7),
            synthetic_idle(&target, 300),
        ];
        Arc::new(Database::build(&target, &clips, &BuildConfig::default()).unwrap())
    }

    #[test]
    fn standing_still_stays_put() {
        let mut m = MotionMatcher::new(db(), Config::default(), Vec3::ZERO, 0.0);
        let s = ConstantSteering {
            velocity: Vec3::ZERO,
            facing: 0.0,
        };
        for _ in 0..240 {
            m.step(1.0 / 60.0, &s);
        }
        assert!(
            m.root().translation.length() < 0.25,
            "{:?}",
            m.root().translation
        );
        assert!(m.world_positions().iter().all(|p| p.is_finite()));
    }

    #[test]
    fn walks_where_it_is_steered() {
        let mut m = MotionMatcher::new(db(), Config::default(), Vec3::ZERO, 0.0);
        let path = PathSteering::new(&[Vec3::ZERO, Vec3::new(0.0, 0.0, 6.0)], 1.0);
        for _ in 0..(60 * 10) {
            m.step(1.0 / 60.0, &path);
        }
        // The synthetic database has walks and an idle but no stopping motion, so it runs a
        // little past the end; the steering stops it there rather than turning it back.
        let p = m.root().translation;
        assert!((p - Vec3::new(0.0, 0.0, 6.0)).length() < 0.5, "{p:?}");
        assert!(m.stats().searches > 10);
    }

    #[test]
    fn any_frame_rate_is_finite_and_follows() {
        for hz in [30.0f32, 75.0, 144.0] {
            let mut m = MotionMatcher::new(db(), Config::default(), Vec3::ZERO, 0.0);
            let s = ConstantSteering {
                velocity: Vec3::new(0.0, 0.0, 0.9),
                facing: 0.0,
            };
            for _ in 0..(hz as usize * 5) {
                m.step(1.0 / hz, &s);
            }
            assert!(
                m.root().translation.z > 3.0,
                "{hz} Hz: {:?}",
                m.root().translation
            );
            assert!(m.world_positions().iter().all(|p| p.is_finite()));
        }
    }
}
