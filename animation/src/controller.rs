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

use crate::database::{
    tags, Database, FEATURES, HORIZON_SECONDS, LEFT_BALL, LEFT_HEEL, RIGHT_BALL, RIGHT_HEEL,
};
use crate::ik::two_bone;
use crate::math::{wrap_angle, Quat, Transform, Vec3};
use crate::path::Steering;
use crate::search::{search, search_features, Exclusions, Match, SearchStats};
use crate::spring::{angular_velocity, InertialQuat, InertialVec3, RotationSpring, VelocitySpring};

/// Tuning of the runtime. The defaults suit a walking humanoid at 30-240 Hz.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    /// Seconds between searches.
    pub search_interval: f32,
    /// Half-life of transition blends (s).
    pub blend_halflife: f32,
    /// The inertialization half-life (s) for the first moments after `take_over`: the body
    /// handed over differs from the capture by more than a transition between frames does.
    pub takeover_halflife: f32,
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
    /// Keep holding a planted point after the capture's contact ends while the animated point
    /// is still within this height (m) of where it stands on the floor: a foot lets go only
    /// once it lifts. A capture's turning start drags a toe round on the floor before it lifts
    /// (its contact ends at 0.3 m/s); held until it lifts, it pivots in the air instead. 0: off
    /// (the capture's contacts alone).
    pub hold_until_lift: f32,
    /// Every held foot keeps the heading (ankle to toe on the floor) it came down with until it
    /// lets go, then eases to the capture's in the air: a sole never turns on the floor (a
    /// capture's pivot as it turns on a planted foot slides its sole round the held point). Off:
    /// only a foot a take-over planted keeps how far it stood turned from the capture.
    pub keep_turn_planted: bool,
    /// The legs' soft reach: the last fraction of a leg's length approached slowly.
    pub soft_reach: f32,
    /// Seconds either side of the current frame the search skips.
    pub skip_surrounding: f32,
    /// Seconds before a clip's end the search never lands on.
    pub end_margin: f32,
    /// The clips the search draws from (`database::tags`), and those it never uses (for a
    /// man, `WOMAN`; for a woman, `MAN`).
    pub search_tags: u32,
    pub exclude_tags: u32,
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
    /// Seconds a still character may lag its simulation before the search drops the pose.
    pub stuck_after: f32,
    /// Within this distance (m) of the steering's goal (a path's end), bring the character
    /// onto it: the simulation stays on the goal rather than settling onto the character, the
    /// root is pulled towards it at `arrive_ratio` of its own speed, and standing, at most
    /// `arrive_creep` m/s. 0: off (a stop lands where its capture takes it).
    pub arrive_exact: f32,
    /// While nobody wants the character to move, search only frames with these tags (for
    /// example idle and start-stop: a walk clip's standing ends, played as an idle, carry
    /// whatever its performer did with their arms there). 0: no limit.
    pub rest_tags: u32,
    /// ...and never frames with these (for example turn: turning on the spot reads as
    /// fidgeting). Both apply only once the character also faces about the way wanted.
    pub rest_exclude: u32,
    /// Standing calmly, the inertialization half-life (s) is at least this: the pose features
    /// hold the feet and hips but not the arms, so a transition between two idles can turn an
    /// arm a long way, and at 0.1 s it whips. 0: the usual half-life.
    pub rest_halflife: f32,
    /// Paths walked by a `Character` take their end's facing within this distance of the end
    /// (`PathSteering::face_from`). 0: only once stopped.
    pub face_from: f32,
    pub arrive_ratio: f32,
    pub arrive_creep: f32,
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
            "takeover_halflife" => self.takeover_halflife = f()?,
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
            "hold_until_lift" => self.hold_until_lift = f()?,
            "keep_turn_planted" => self.keep_turn_planted = b()?,
            "soft_reach" => self.soft_reach = f()?,
            "skip_surrounding" => self.skip_surrounding = f()?,
            "end_margin" => self.end_margin = f()?,
            "search_tags" => self.search_tags = u()? as u32,
            "exclude_tags" => self.exclude_tags = u()? as u32,
            "force_search_velocity" => self.force_search_velocity = f()?,
            "force_search_facing" => self.force_search_facing = f()?,
            "transition_cost" => self.transition_cost = f()?,
            "blend_root_velocity" => self.blend_root_velocity = b()?,
            "settle_halflife" => self.settle_halflife = f()?,
            "settle_facing" => self.settle_facing = f()?,
            "simulate_from_character" => self.simulate_from_character = b()?,
            "time_warp" => self.time_warp = f()?,
            "stuck_after" => self.stuck_after = f()?,
            "arrive_exact" => self.arrive_exact = f()?,
            "rest_tags" => self.rest_tags = f()? as u32,
            "rest_exclude" => self.rest_exclude = f()? as u32,
            "rest_halflife" => self.rest_halflife = f()?,
            "face_from" => self.face_from = f()?,
            "arrive_ratio" => self.arrive_ratio = f()?,
            "arrive_creep" => self.arrive_creep = f()?,
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
            takeover_halflife: 0.25,
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
            hold_until_lift: 0.0,
            keep_turn_planted: false,
            unlock_halflife: 0.1,
            soft_reach: 0.03,
            skip_surrounding: 1.0 / 3.0,
            end_margin: 1.0 / 3.0,
            search_tags: tags::LOCOMOTION,
            exclude_tags: 0,
            force_search_velocity: 0.25,
            force_search_facing: 0.35,
            transition_cost: 0.5,
            blend_root_velocity: true,
            settle_halflife: 0.0,
            settle_facing: 0.45,
            simulate_from_character: true,
            time_warp: 0.0,
            stuck_after: 0.4,
            arrive_exact: 0.0,
            rest_tags: 0,
            rest_exclude: 0,
            rest_halflife: 0.0,
            face_from: 0.0,
            arrive_ratio: 0.8,
            arrive_creep: 0.06,
        }
    }
}

/// A body taken over while it walks (`MotionMatcher::take_over_moving`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Moving {
    /// The body's velocity over the ground (world, m/s).
    pub velocity: Vec3,
    /// Each ankle's velocity (world, m/s): left, right.
    pub ankle_velocities: [Vec3; 2],
    /// Which feet stand planted (held where they are): left, right.
    pub planted: [bool; 2],
}

/// A clip played as it is instead of matched: sitting down, a seated loop, standing up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Action {
    /// Database frames `[start, end)`, within one clip.
    pub start: usize,
    pub end: usize,
    /// Start again at `start` on reaching `end` (blended), until another action or a stop.
    pub looping: bool,
    /// Keep the root still (seated): the clip's own root motion is dropped.
    pub pin_root: bool,
    /// Where the root should be when the action ends, warped in over the action's length.
    pub root_target: Option<Transform>,
    /// The hips' height over the floor the action should end at (m), warped in likewise.
    pub hips_height_target: Option<f32>,
}

#[derive(Debug, Clone, Copy)]
struct ActionState {
    action: Action,
    eased: f32,
    lift_from: f32,
    lift_to: f32,
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
    /// Searches on the trajectory alone after the character would not set off.
    pub unstuck: u64,
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
    /// Asked to be planted here (the ankle's place on the ground) on the next step: see `plant_feet`.
    plant: Option<Vec3>,
    /// How far above the floor the held point still is (m), easing to 0 since it was taken.
    settle: f32,
    /// The heading on the floor (ankle to toe, radians) of a foot a take-over planted, while it
    /// stays down: as it stood, turned on since only as the capture itself turns that foot (a
    /// pivot as it sets off turning), never by the take-over's ease or a transition's.
    turn: Option<f32>,
    /// The capture's own heading of that foot last step (radians).
    capture_was: f32,
    /// The turn added to the capture's foot (radians): the kept turn's difference while held,
    /// eased away once the foot lifts.
    twist: f32,
}

/// The half-life (s) over which a newly held foot comes down onto the floor.
const LOCK_SETTLE_HALFLIFE: f32 = 0.05;

impl Default for FootLock {
    fn default() -> Self {
        Self {
            held: None,
            released: None,
            correction: Vec3::ZERO,
            previous_correction: Vec3::ZERO,
            release: InertialVec3::default(),
            plant: None,
            settle: 0.0,
            turn: None,
            capture_was: 0.0,
            twist: 0.0,
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
    action: Option<ActionState>,
    /// Set on the step an action without looping ends.
    action_finished: bool,
    /// How long the simulation has led a character that does not move (s).
    stuck: f32,
    /// Transitions counted as of the last foot lock (lock_feet: a transition's step is not the
    /// capture turning a foot).
    transitions_seen: u64,
    /// Added to the hips' height (m): an action's warp, kept after it ends (seated).
    hips_lift: f32,
    /// Seconds left of the slower inertialization after `take_over`.
    taken_over: f32,
    /// The goal being landed on this step (`Config::arrive_exact`).
    arriving: Option<Vec3>,
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
            action: None,
            action_finished: false,
            stuck: 0.0,
            transitions_seen: 0,
            hips_lift: 0.0,
            taken_over: 0.0,
            arriving: None,
            db,
        };
        // Start on the frame whose trajectory best matches standing still here (there is
        // no pose yet to match, so the pose features are left out).
        m.build_query();
        let mut best = (f32::INFINITY, 0usize);
        for f in 0..m.db.frame_count() {
            if f + m.frames(m.config.end_margin) >= m.clip_end(f)
                || m.db.bounds.frame_tags[f] & m.config.search_tags == 0
                || m.db.bounds.frame_tags[f] & m.config.exclude_tags != 0
            {
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

    /// Starts `action` at once, blended in from the pose now. While it plays there is no
    /// search; the steering is ignored.
    pub fn play(&mut self, action: Action) {
        let db = Arc::clone(&self.db);
        let end = action
            .end
            .min(self.clip_end(action.start))
            .max(action.start + 2);
        let action = Action { end, ..action };
        self.transition(action.start);
        let last = end - 1;
        // A hold lifts by its first frame (a loop cut from a capture may end rising); a
        // one-off by its last, where it should land.
        let lift_to = action.hips_height_target.map_or(0.0, |h| {
            h - db.hips_pos[if action.looping { action.start } else { last }].y
        });
        if action.looping {
            // A hold (seated): its lift applies at once, and the hips' inertialization hides
            // the step as it hides the clip's own, so the shown hips stay where they were.
            // (Eased over the loop instead, two clips' different seat heights would show as
            // a bob.)
            self.hips_blend.offset.y += self.hips_lift - lift_to;
            self.hips_lift = lift_to;
        }
        self.action = Some(ActionState {
            action,
            eased: 0.0,
            lift_from: self.hips_lift,
            lift_to,
        });
        self.action_finished = false;
        self.search_timer = self.config.search_interval;
    }

    /// Stops a looping action: matching resumes from the pose now.
    pub fn stop_action(&mut self) {
        self.action = None;
    }

    /// The action playing, if any, and how far through it is (0 to 1).
    #[must_use]
    pub fn action(&self) -> Option<(Action, f32)> {
        self.action.map(|a| {
            let span = (a.action.end - a.action.start).max(2) - 1;
            (
                a.action,
                ((self.frame - a.action.start) as f32 + self.frac) / span as f32,
            )
        })
    }

    /// Whether an action without looping ended on the last step.
    #[must_use]
    pub fn action_finished(&self) -> bool {
        self.action_finished
    }

    /// The hips' height added by actions (m).
    #[must_use]
    pub fn hips_lift(&self) -> f32 {
        self.hips_lift
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

    /// Plants the feet where a character placed mid-scene has them (each ankle's place on the
    /// ground, world; `None` leaves a foot to the capture): from the next step the foot lock holds
    /// a foot the pose has on the ground there, and eases one it lifts back to the pose from there,
    /// so a body handed over from other animation keeps its feet where they stood.
    pub fn plant_feet(&mut self, ankles: [Option<Vec3>; 2]) {
        for (f, a) in self.feet.iter_mut().zip(ankles) {
            f.plant = a;
        }
    }

    /// Takes a body over from other animation without a jump: `root` on the ground, `hips`
    /// relative to it and `rotations` of the driven bones as `local_rotations` gives them (the
    /// hips' relative to the root, the others to their parents). Playback continues from the
    /// database frame whose feet (relative to the root) are most like this pose's, among frames
    /// the search may use that stand or set off (idle, start-stop, walk, turn); the difference
    /// is eased away by inertialization (`Config::takeover_halflife` for the first half second),
    /// the feet planted where they stand and the simulation at rest under the root.
    pub fn take_over(&mut self, root: Transform, hips: Vec3, rotations: &[Quat]) {
        self.take_over_with(root, hips, rotations, None);
    }

    /// Takes over a body that is walking: as [`take_over`](Self::take_over), with the body's
    /// ground `velocity` (world, m/s) and each ankle's (world), and which feet stand planted.
    /// Playback continues from the frame among walking, turning and starting frames that best
    /// fits this pose, these velocities and a straight path on at this velocity (the whole
    /// feature set); the simulation and the root carry on at this velocity; the planted feet are
    /// held where they stand, the others left to the capture.
    pub fn take_over_moving(
        &mut self,
        root: Transform,
        hips: Vec3,
        rotations: &[Quat],
        moving: Moving,
    ) {
        self.take_over_with(root, hips, rotations, Some(moving));
    }

    fn take_over_with(
        &mut self,
        root: Transform,
        hips: Vec3,
        rotations: &[Quat],
        moving: Option<Moving>,
    ) {
        let db = Arc::clone(&self.db);
        let b = db.bone_count();
        // This pose's ankles relative to the root, by forward kinematics from the rest offsets.
        self.world_pos[0] = hips;
        self.world_rot[0] = rotations.first().copied().unwrap_or(Quat::IDENTITY);
        for i in 1..b {
            let p = db.skeleton.parents[i].unwrap_or(0);
            let r = rotations.get(i).copied().unwrap_or(db.rotations[i]);
            self.world_pos[i] = self.world_pos[p] + self.world_rot[p].rotate(self.local_pos[i]);
            self.world_rot[i] = self.world_rot[p] * r;
        }
        let legs = db.humanoid.legs;
        let ankles = [self.world_pos[legs[0].ankle], self.world_pos[legs[1].ankle]];
        // The query: these feet; standing, at rest (no foot or hip velocity) and going nowhere;
        // walking, at their velocities and going on straight at the body's.
        let velocity = moving.map_or(Vec3::ZERO, |m| m.velocity.flat());
        self.root = root;
        self.sim_pos = VelocitySpring {
            position: root.translation.flat(),
            velocity,
            ..VelocitySpring::default()
        };
        self.sim_rot = RotationSpring {
            rotation: root.rotation,
            angular_velocity: Vec3::ZERO,
        };
        for (k, h) in HORIZON_SECONDS.iter().enumerate() {
            self.trajectory[k] = (root.translation.flat() + velocity * *h, root.rotation);
        }
        self.build_query();
        for (side, a) in ankles.iter().enumerate() {
            for (c, v) in [a.x, a.y, a.z].into_iter().enumerate() {
                let k = 3 * side + c;
                self.query[k] = (v - db.feature_mean[k]) / db.feature_scale[k];
            }
        }
        let in_root = |v: Vec3| root.rotation.inverse_rotate(v);
        let (lv, rv) = moving.map_or((Vec3::ZERO, Vec3::ZERO), |m| {
            (
                in_root(m.ankle_velocities[0]),
                in_root(m.ankle_velocities[1]),
            )
        });
        let hv = in_root(velocity);
        for (k, v) in [lv.x, lv.y, lv.z, rv.x, rv.y, rv.z, hv.x, hv.y, hv.z]
            .into_iter()
            .enumerate()
        {
            let k = 6 + k;
            self.query[k] = (v - db.feature_mean[k]) / db.feature_scale[k];
        }
        let wanted = if moving.is_some() {
            tags::START_STOP | tags::WALK | tags::TURN
        } else {
            tags::IDLE | tags::START_STOP | tags::WALK | tags::TURN
        };
        // Through the search's bounding boxes, as any search (a frame by frame scan of every frame
        // took 10-12 ms once on a Raspberry Pi 5, in the frame a walk was taken over).
        let ex = Exclusions {
            end_margin: self.frames(self.config.end_margin),
            skip: (0, 0),
            tags: wanted & self.config.search_tags,
            exclude: self.config.exclude_tags,
        };
        let mut st = SearchStats::default();
        let to = search(&db, &self.query, None, ex, &mut st).map_or(self.frame, |m| m.frame);
        // Shown as given; the capture's frame underneath.
        for i in 0..b {
            let shown = rotations
                .get(i)
                .copied()
                .unwrap_or(db.rotations[to * b + i]);
            self.bone_blend[i] = InertialQuat {
                offset: (shown * db.rotations[to * b + i].inverse())
                    .abs()
                    .normalize(),
                offset_vel: Vec3::ZERO,
            };
        }
        self.hips_blend = InertialVec3 {
            offset: hips - db.hips_pos[to],
            offset_vel: Vec3::ZERO,
        };
        self.root_vel_blend = InertialVec3::default();
        self.yaw_rate_blend = InertialVec3::default();
        self.root_velocity = velocity;
        self.root_yaw_rate = 0.0;
        self.frame = to;
        self.frac = 0.0;
        self.anchor = root.compose(self.db_root(to, 0.0).inverse());
        if moving.is_some() && self.config.blend_root_velocity {
            // Walking on at the body's own velocity, eased into the capture's.
            let t1 = (to + 1).min(self.clip_end(to) - 1);
            let v_new = self
                .anchor
                .rotation
                .rotate((db.root_pos[t1] - db.root_pos[to]) * db.fps);
            self.root_vel_blend
                .transition(velocity, Vec3::ZERO, v_new, Vec3::ZERO);
        }
        self.action = None;
        self.action_finished = false;
        self.hips_lift = 0.0;
        self.stuck = 0.0;
        self.search_timer = 0.0;
        // A standing body differs from the capture by more than a transition does (the long
        // half-life); a walking one, taken over on the capture's moment most like it, by about a
        // transition's (eased over the long one, its swinging foot was blended between two strides
        // and landed sliding).
        self.taken_over = if moving.is_some() { 0.0 } else { 0.5 };
        self.stats.transitions += 1;
        let r = root.rotation;
        let planted = moving.map_or([true, true], |m| m.planted);
        // Whatever the feet were held at before (an earlier walk's last steps) is let go: a foot
        // not planted now is the capture's, not pulled back to where it last stood.
        for f in &mut self.feet {
            *f = FootLock::default();
        }
        self.plant_feet([
            planted[0].then(|| root.translation + r.rotate(ankles[0])),
            planted[1].then(|| root.translation + r.rotate(ankles[1])),
        ]);
        self.sample(to, 0.0);
        for i in 0..b {
            self.out_rot[i] = self.bone_blend[i].apply(self.anim_rot[i]);
        }
        self.out_hips = hips;
        self.pose_world();
    }

    /// `seconds` in database frames.
    fn frames(&self, seconds: f32) -> usize {
        (seconds * self.db.fps).round() as usize
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
        for (k, h) in HORIZON_SECONDS.iter().enumerate() {
            let until = *h;
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
        self.action_finished = false;
        if self.action.is_some() {
            self.sim_pos = VelocitySpring {
                position: self.root.translation.flat(),
                ..VelocitySpring::default()
            };
            self.sim_rot = RotationSpring {
                rotation: self.root.rotation,
                angular_velocity: Vec3::ZERO,
            };
        }
        let (desired_v, mut desired_f) = if self.action.is_some() {
            (Vec3::ZERO, self.root.rotation.yaw())
        } else {
            steering.desired(self.sim_pos.position)
        };
        let resting = desired_v.flat().length() < 0.05 && self.sim_pos.velocity.length() < 0.05;
        // Standing, and already facing about the way wanted: keep the facing, rather than
        // shuffle round the last few degrees.
        let root_yaw = self.root.rotation.yaw();
        let calm = resting && wrap_angle(desired_f - root_yaw).abs() < cfg.settle_facing;
        if calm {
            desired_f = root_yaw;
        }
        self.sim_pos
            .update(desired_v.flat(), cfg.velocity_halflife, dt);
        // Near the goal it is to land on (arrive_exact), the simulation stays on it.
        let goal = steering.goal().filter(|g| {
            self.action.is_none()
                && cfg.arrive_exact > 0.0
                && (*g - self.root.translation).flat().length() < cfg.arrive_exact
        });
        self.arriving = goal;
        if let (true, Some(g)) = (resting, goal) {
            self.sim_pos.position = g.flat();
            self.sim_pos.velocity = Vec3::ZERO;
        } else if resting {
            // At rest, with nobody wanting to move, the simulation settles onto the
            // character: a few centimetres short of a goal is a person standing, not a cue
            // to shuffle.
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
        if let Some(st) = self.action {
            if end_of_clip || self.frame + 1 >= st.action.end {
                if st.action.looping {
                    self.transition(st.action.start);
                    // Later loops hold the target the first one reached.
                    self.action = Some(ActionState {
                        eased: 0.0,
                        lift_from: st.lift_to,
                        ..st
                    });
                } else {
                    self.action = None;
                    self.action_finished = true;
                    self.hips_lift = st.lift_to;
                }
            }
        }
        // Led for a while and still not going: the pose holds it to standing frames (no data
        // to set off from this pose), so search on the trajectory alone and blend the jump.
        let lead = (self.sim_pos.position - self.root.translation)
            .flat()
            .length();
        if self.action.is_none() && lead > 0.12 && self.root_velocity.length() < 0.1 {
            self.stuck += dt;
        } else {
            self.stuck = 0.0;
        }
        if self.stuck > cfg.stuck_after {
            self.stuck = 0.0;
            self.build_query();
            // The trajectory's features only (the pose dropped), through the search's bounding
            // boxes: a scan of every frame here was the kind that cost 10-12 ms once on a
            // Raspberry Pi 5 in a take-over's frame.
            let ex = Exclusions {
                end_margin: self.frames(cfg.end_margin),
                skip: (0, 0),
                tags: cfg.search_tags,
                exclude: cfg.exclude_tags,
            };
            let mut st = SearchStats::default();
            let db = Arc::clone(&self.db);
            let found = search_features(&db, &self.query, None, ex, &mut st, 15);
            if let Some(m) = found.filter(|m| m.frame != self.frame) {
                self.transition(m.frame);
                self.stats.unstuck += 1;
            }
            self.search_timer = cfg.search_interval;
        }
        if self.action.is_none() && (self.search_timer <= 0.0 || force || end_of_clip) {
            self.search_timer = cfg.search_interval;
            if force {
                self.last_desired = (desired_v, desired_f);
            }
            self.build_query();
            // Carrying on is a choice only for a frame the search may choose: standing calmly on
            // a walk or a turn (outside `rest_tags`, or in `rest_exclude`), it went on walking
            // (a transitions clip carried a body asked to stand at 0.4-0.6 m/s for seconds, its
            // cost less the transition cost below every idle frame's).
            let tags_now = self.db.bounds.frame_tags[self.frame];
            let resting_off = calm
                && ((cfg.rest_tags != 0 && tags_now & cfg.rest_tags == 0)
                    || tags_now & cfg.rest_exclude != 0);
            let current = if end_of_clip || resting_off {
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
                    .saturating_sub(self.frames(cfg.skip_surrounding))
                    .max(clip_start),
                (self.frame + self.frames(cfg.skip_surrounding)).min(self.clip_end(self.frame)),
            );
            let ex = Exclusions {
                end_margin: self.frames(cfg.end_margin),
                skip: if end_of_clip { (0, 0) } else { skip },
                tags: if calm && cfg.rest_tags != 0 {
                    cfg.search_tags & cfg.rest_tags
                } else {
                    cfg.search_tags
                },
                exclude: if calm {
                    cfg.exclude_tags | cfg.rest_exclude
                } else {
                    cfg.exclude_tags
                },
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
        let mut pose_halflife = if self.taken_over > 0.0 {
            cfg.takeover_halflife.max(cfg.blend_halflife)
        } else {
            cfg.blend_halflife
        };
        if calm {
            pose_halflife = pose_halflife.max(cfg.rest_halflife);
        }
        self.taken_over = (self.taken_over - dt).max(0.0);
        for i in 0..b {
            self.out_rot[i] = self.bone_blend[i].update(self.anim_rot[i], pose_halflife, dt);
        }
        let next = (self.frame + 1).min(self.clip_end(self.frame) - 1);
        let hips_anim = self.db.hips_pos[self.frame].lerp(self.db.hips_pos[next], self.frac);
        self.out_hips = self.hips_blend.update(hips_anim, pose_halflife, dt);
        self.out_hips.y += self.hips_lift;

        // 5. Root: animation root motion, blended velocity, nudged towards the simulation.
        let anim_vel = delta_pos / dt;
        let vel = self.root_vel_blend.update(anim_vel, cfg.blend_halflife, dt);
        let yaw_rate = self
            .yaw_rate_blend
            .update(Vec3::new(delta_yaw / dt, 0.0, 0.0), cfg.blend_halflife, dt)
            .x;
        let mut pos = self.root.translation + vel * dt;
        let mut yaw = self.root.rotation.yaw() + yaw_rate * dt;
        if let Some(mut st) = self.action {
            // An action: its own root motion (none when pinned), warped onto its target, and
            // the hips' height eased from where it was to where the action ends.
            if st.action.pin_root {
                pos = self.root.translation;
                yaw = self.root.rotation.yaw();
            }
            let span = ((st.action.end - st.action.start).max(2) - 1) as f32;
            let u = (((self.frame - st.action.start) as f32 + self.frac) / span).clamp(0.0, 1.0);
            let eased = u * u * (3.0 - 2.0 * u);
            let d = eased - st.eased;
            if let Some(target) = st.action.root_target {
                // Closed loop: where the clip's own motion ends from here (none when pinned),
                // and a share of what is left, so the action ends on its target whatever the
                // velocity blend carried in.
                let rest = if st.action.pin_root {
                    Transform::IDENTITY
                } else {
                    self.db_root(self.frame, self.frac)
                        .inverse()
                        .compose(self.db_root(st.action.end - 1, 0.0))
                };
                let end = Transform::new(pos.flat(), Quat::from_yaw(yaw)).compose(rest);
                let share = if st.eased < 1.0 - 1e-4 {
                    (d / (1.0 - st.eased)).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                pos += (target.translation - end.translation).flat() * share;
                yaw += wrap_angle(target.rotation.yaw() - end.rotation.yaw()) * share;
            }
            st.eased = eased;
            self.hips_lift = st.lift_from + (st.lift_to - st.lift_from) * eased;
            self.action = Some(st);
        } else {
            // Adjustment, limited by the root's own speed.
            let to_sim = (self.sim_pos.position - pos).flat();
            let pull = to_sim
                * (1.0 - (-std::f32::consts::LN_2 * dt / cfg.adjust_position_halflife).exp());
            let max_pull = match self.arriving {
                // Landing on a goal: pulled harder while moving, and creeping onto it standing.
                Some(_) => (cfg.arrive_ratio * vel.length() + cfg.arrive_creep) * dt,
                None => cfg.adjust_velocity_ratio * vel.length() * dt,
            };
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
                    self.sim_pos.position =
                        pos.flat() - off.normalize_or_zero() * cfg.clamp_distance;
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

    /// The heading on the floor of `side`'s ankle to its toe as the capture alone has it (its
    /// sampled rotations, without the transitions' ease), radians.
    fn capture_turn(&self, side: usize) -> f32 {
        let db = &self.db;
        let leg = db.humanoid.legs[side];
        let mut chain = [0usize; 32];
        let (mut n, mut i) = (0, leg.ankle);
        loop {
            if n < chain.len() {
                chain[n] = i;
                n += 1;
            }
            match db.skeleton.parents[i] {
                Some(p) => i = p,
                None => break,
            }
        }
        let mut r = self.root.rotation;
        for k in (0..n).rev() {
            r = r * self.anim_rot[chain[k]];
        }
        let d = r.rotate(self.local_pos[leg.toe]).flat();
        d.x.atan2(d.z)
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
        let raw_turn = [self.capture_turn(0), self.capture_turn(1)];
        let contacts = self.db.contacts[self.frame];
        let heights = self.db.foot_heights;
        for (side, &capture_turn) in raw_turn.iter().enumerate() {
            let leg = self.db.humanoid.legs[side];
            let (heel_bit, ball_bit) = if side == 0 {
                (LEFT_HEEL, LEFT_BALL)
            } else {
                (RIGHT_HEEL, RIGHT_BALL)
            };
            let ankle_in = self.world_pos[leg.ankle];
            let toe_raw = self.world_pos[leg.toe];
            let foot = &mut self.feet[side];
            // A foot a take-over planted keeps how far it stood turned from the capture while it
            // stays down (eased from the shown pose to the capture's, a planted foot turned on the
            // floor about its held point and its sole slid: 3-5 cm in a game's rise from a chair);
            // once it lifts, the difference eases away in the air.
            let along = (toe_raw - ankle_in).flat();
            let anim_turn = along.x.atan2(along.z);
            // The capture's own turn of the foot this step: none across a transition (its sampled
            // pose jumps there, and the ease hides it), nor more than a pivot can (6 degrees a step).
            let mut own = wrap_angle(capture_turn - foot.capture_was);
            if self.stats.transitions != self.transitions_seen || own.abs() > 0.1 {
                own = 0.0;
            }
            foot.capture_was = capture_turn;
            foot.twist = match (foot.held, foot.turn.as_mut()) {
                (Some(_), Some(turn)) => {
                    if !cfg.keep_turn_planted {
                        *turn = wrap_angle(*turn + own);
                    }
                    wrap_angle(*turn - anim_turn)
                }
                _ => {
                    foot.turn = None;
                    foot.twist * (-std::f32::consts::LN_2 * dt / cfg.unlock_halflife).exp()
                }
            };
            let twist = Quat::from_yaw(foot.twist);
            // The toe as the foot is shown, turned about the ankle.
            let toe_in = ankle_in + twist.rotate(toe_raw - ankle_in);
            // The hips' lift (a seat lower or higher than the capture's) moves the whole body;
            // a foot the capture had on or near the floor goes back down (or up) to it.
            let captured = ankle_in.y - self.hips_lift - heights[0];
            let near_floor = (1.0 - captured / 0.25).clamp(0.0, 1.0);
            let ground = Vec3::new(0.0, -self.hips_lift * near_floor, 0.0);
            let want = if contacts & ball_bit != 0 {
                Some(Point::Ball)
            } else if contacts & heel_bit != 0 {
                Some(Point::Heel)
            } else {
                None
            };
            if foot.released.is_some() && foot.released != want {
                foot.released = None;
            }
            let want = if foot.released.is_some() { None } else { want };
            let point_in = |p: Point| if p == Point::Heel { ankle_in } else { toe_in };
            // Held on until the foot lifts (hold_until_lift), the capture's contact over or not:
            // by the ball while it is still down (a heel up, the foot pivoting on its ball), else
            // the heel while it is.
            let want = match (want, foot.held) {
                (None, Some(_)) if cfg.hold_until_lift > 0.0 => {
                    let low = |p: Point, floor: f32| {
                        point_in(p).y - self.hips_lift * near_floor - floor < cfg.hold_until_lift
                    };
                    if low(Point::Ball, heights[1]) {
                        Some(Point::Ball)
                    } else if low(Point::Heel, heights[0]) {
                        Some(Point::Heel)
                    } else {
                        None
                    }
                }
                (w, _) => w,
            };
            if let Some(p) = foot.plant.take() {
                // Taken up where it was planted: held there if the pose has it down, else let go
                // from there.
                let c = Vec3::new(p.x - ankle_in.x, 0.0, p.z - ankle_in.z);
                foot.released = None;
                foot.correction = c;
                foot.previous_correction = c;
                match want {
                    Some(w) => {
                        let mut at = point_in(w) + c;
                        at.y = if w == Point::Heel {
                            heights[0]
                        } else {
                            heights[1]
                        };
                        foot.held = Some((w, at));
                        foot.turn = Some(wrap_angle(anim_turn + foot.twist));
                    }
                    None => {
                        foot.held = None;
                        foot.release.offset = c;
                        foot.release.offset_vel = Vec3::ZERO;
                    }
                }
            }
            let changed = match (foot.held, want) {
                (Some((p, _)), Some(w)) => p != w,
                (None, None) => false,
                _ => true,
            };
            if changed {
                match want {
                    Some(w) => {
                        if cfg.keep_turn_planted && foot.held.is_none() {
                            foot.turn = Some(wrap_angle(anim_turn + foot.twist));
                        }
                        let mut at = point_in(w) + foot.correction;
                        let shown = at.y;
                        at.y = if w == Point::Heel {
                            heights[0]
                        } else {
                            heights[1]
                        };
                        // Down onto the floor over a moment, not in a frame: a heel held 4 cm
                        // above it pulled the knee down 9 cm at once.
                        foot.settle = shown - at.y;
                        foot.held = Some((w, at));
                    }
                    None => {
                        foot.release.offset = foot.correction - ground;
                        foot.release.offset_vel = (foot.correction - foot.previous_correction) / dt;
                        foot.held = None;
                    }
                }
            }
            foot.settle *= (-std::f32::consts::LN_2 * dt / LOCK_SETTLE_HALFLIFE).exp();
            let correction = match foot.held {
                Some((p, at)) => {
                    let c = at - point_in(p) + Vec3::new(0.0, foot.settle, 0.0);
                    if (c - ground).length() > cfg.unlock_radius {
                        foot.release.offset = foot.correction - ground;
                        foot.release.offset_vel = Vec3::ZERO;
                        foot.released = Some(p);
                        foot.held = None;
                        self.stats.unlocks_by_radius += 1;
                        foot.release.update(Vec3::ZERO, cfg.unlock_halflife, dt) + ground
                    } else {
                        c
                    }
                }
                None => foot.release.update(Vec3::ZERO, cfg.unlock_halflife, dt) + ground,
            };
            foot.previous_correction = foot.correction;
            foot.correction = correction;
            if correction.length_squared() < 1e-10 && foot.twist.abs() < 1e-5 {
                continue;
            }
            // Two-bone IK puts the ankle at its corrected place; the foot keeps its turn (and a
            // take-over's planted foot the turn it stood with: twist, about the vertical).
            let target = ankle_in + correction;
            let ankle_world_rot = (twist * self.world_rot[leg.ankle]).normalize();
            let toe_world_rot = (twist * self.world_rot[leg.toe]).normalize();
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
        self.transitions_seen = self.stats.transitions;
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
    fn a_take_over_finds_the_frame_a_full_scan_would() {
        // Through the bounding boxes, the same frame as testing every frame (moving and standing).
        let db = db();
        let b = db.bone_count();
        for (f, moving) in [(100usize, true), (700, true), (1300, false), (450, false)] {
            let mut m = MotionMatcher::new(Arc::clone(&db), Config::default(), Vec3::ZERO, 0.0);
            let rot: Vec<Quat> = (0..b).map(|i| db.rotations[f * b + i]).collect();
            let root = Transform::new(Vec3::new(1.0, 0.0, 2.0), Quat::from_yaw(0.3));
            if moving {
                let v = root.rotation.rotate(Vec3::new(0.0, 0.0, 0.9));
                m.take_over_moving(
                    root,
                    db.hips_pos[f],
                    &rot,
                    Moving {
                        velocity: v,
                        ankle_velocities: [Vec3::ZERO, v * 2.0],
                        planted: [true, false],
                    },
                );
            } else {
                m.take_over(root, db.hips_pos[f], &rot);
            }
            let wanted = if moving {
                tags::START_STOP | tags::WALK | tags::TURN
            } else {
                tags::IDLE | tags::START_STOP | tags::WALK | tags::TURN
            };
            let ex = Exclusions {
                end_margin: m.frames(m.config.end_margin),
                skip: (0, 0),
                tags: wanted & m.config.search_tags,
                exclude: m.config.exclude_tags,
            };
            let full = crate::search::search_brute_force(&db, &m.query, ex).map(|x| x.frame);
            assert_eq!(Some(m.frame()), full, "taken over from frame {f}");
        }
    }

    #[test]
    fn standing_calmly_never_carries_on_a_walk() {
        // Walking, then asked to stand: with rest tags (idle) set, the next search leaves the walk
        // for an idle frame, however much carrying on is favoured (a huge transition cost).
        let config = Config {
            rest_tags: tags::IDLE,
            transition_cost: 1.0e6,
            ..Config::default()
        };
        let target = humanoid_skeleton(1.0);
        let clips = [
            synthetic_walk(&target, 3000, 1.0),
            synthetic_idle(&target, 300),
        ];
        let db = Arc::new(Database::build(&target, &clips, &BuildConfig::default()).unwrap());
        let mut m = MotionMatcher::new(db, config, Vec3::ZERO, 0.0);
        let walk = ConstantSteering {
            velocity: Vec3::new(0.0, 0.0, 1.0),
            facing: 0.0,
        };
        for _ in 0..90 {
            m.step(1.0 / 60.0, &walk);
        }
        assert!(m.db.bounds.frame_tags[m.frame()] & tags::WALK != 0);
        let stand = ConstantSteering {
            velocity: Vec3::ZERO,
            facing: 0.0,
        };
        for _ in 0..180 {
            m.step(1.0 / 60.0, &stand);
        }
        let t = m.db.bounds.frame_tags[m.frame()];
        assert!(
            t & tags::IDLE != 0,
            "standing, it plays frame {} (tags {t:#x})",
            m.frame()
        );
    }

    #[test]
    fn take_over_starts_from_the_pose_shown_and_eases_into_the_capture() {
        let db = db();
        let b = db.bone_count();
        let mut m = MotionMatcher::new(Arc::clone(&db), Config::default(), Vec3::ZERO, 0.0);
        // Another animation's pose: a walking frame's, every bone turned a little more.
        let f = 120;
        let twist = Quat::from_axis_angle(Vec3::X, 0.08);
        // (The legs as they are, so the feet stand on the floor as another animation's would.)
        let leg = |i: usize| {
            ["Leg", "Foot", "Toe"]
                .iter()
                .any(|n| db.skeleton.names[i].contains(n))
        };
        let shown: Vec<Quat> = (0..b)
            .map(|i| {
                let q = db.rotations[f * b + i];
                if leg(i) || i == 0 {
                    q
                } else {
                    (q * twist).normalize()
                }
            })
            .collect();
        let hips = db.hips_pos[f] + Vec3::new(0.02, 0.0, 0.02);
        let root = Transform::new(Vec3::new(2.0, 0.0, 1.0), Quat::from_yaw(0.7));
        m.take_over(root, hips, &shown);
        for (i, (got, want)) in m.local_rotations().iter().zip(&shown).enumerate() {
            assert!(got.angle_to(*want) < 1e-3, "bone {i}");
        }
        assert!((m.hips_position() - hips).length() < 1e-4);
        assert!((m.root().translation - root.translation).length() < 1e-5);
        // Standing on: no joint jumps in a frame, and the difference eases away.
        let s = ConstantSteering {
            velocity: Vec3::ZERO,
            facing: 0.7,
        };
        let mut was = m.world_positions().to_vec();
        // (Over the take-over's half second; the synthetic idle's own foot releases come later.)
        for _ in 0..30 {
            m.step(1.0 / 60.0, &s);
            for (p, q) in m.world_positions().iter().zip(&was) {
                assert!(
                    (*p - *q).length() < 0.035,
                    "a joint moved {} m in a frame",
                    (*p - *q).length()
                );
            }
            was.copy_from_slice(m.world_positions());
        }
        for _ in 0..90 {
            m.step(1.0 / 60.0, &s);
        }
        let left = (0..b)
            .map(|i| m.bone_blend[i].offset.angle_to(Quat::IDENTITY))
            .fold(0.0f32, f32::max);
        assert!(left < 0.05, "offset left {left}");
    }

    #[test]
    fn a_take_overs_planted_foot_keeps_the_turn_it_stood_with() {
        let db = db();
        let b = db.bone_count();
        // A standing frame of the synthetic idle, its left foot shown turned 0.35 rad about the
        // vertical (another animation's stance): the foot's world turn by forward kinematics.
        let f = db.clips[2].start + 60;
        let legs = db.humanoid.legs;
        let mut world = vec![Quat::IDENTITY; b];
        for i in 0..b {
            let r = db.rotations[f * b + i];
            world[i] = match db.skeleton.parents[i] {
                Some(p) => world[p] * r,
                None => r,
            };
        }
        let foot = legs[0].ankle;
        let parent = db.skeleton.parents[foot].unwrap();
        let turned = (world[parent].inverse() * Quat::from_yaw(0.35) * world[foot]).normalize();
        let shown: Vec<Quat> = (0..b)
            .map(|i| {
                if i == foot {
                    turned
                } else {
                    db.rotations[f * b + i]
                }
            })
            .collect();
        let heading = |m: &MotionMatcher| {
            let w = m.world_positions();
            let d = (w[legs[0].toe] - w[legs[0].ankle]).flat();
            d.x.atan2(d.z)
        };
        let mut m = MotionMatcher::new(Arc::clone(&db), Config::default(), Vec3::ZERO, 0.0);
        m.take_over(
            Transform::new(Vec3::ZERO, Quat::IDENTITY),
            db.hips_pos[f],
            &shown,
        );
        let shown_heading = heading(&m);
        let toe0 = m.world_positions()[legs[0].toe];
        let s = ConstantSteering {
            velocity: Vec3::ZERO,
            facing: 0.0,
        };
        // Held through the take-over's ease: its heading and its toe stay where they stood.
        for _ in 0..30 {
            m.step(1.0 / 60.0, &s);
            if !m.feet_held()[0] {
                break;
            }
            assert!(
                wrap_angle(heading(&m) - shown_heading).abs() < 0.02,
                "turned {} rad on the floor",
                wrap_angle(heading(&m) - shown_heading)
            );
            assert!((m.world_positions()[legs[0].toe] - toe0).flat().length() < 0.005);
        }
    }

    /// The heading on the floor (ankle to toe) of `side`'s foot as the matcher poses it.
    fn foot_heading(m: &MotionMatcher, side: usize) -> f32 {
        let leg = m.db.humanoid.legs[side];
        let w = m.world_positions();
        let d = (w[leg.toe] - w[leg.ankle]).flat();
        d.x.atan2(d.z)
    }

    #[test]
    fn a_planted_foot_keeps_its_heading_when_asked() {
        // Walking round a circle, the body turns over each planted foot. With keep_turn_planted
        // a held foot's heading never changes on the floor; without it, it turns with the body.
        let turned = |keep: bool| {
            let cfg = Config {
                keep_turn_planted: keep,
                ..Config::default()
            };
            let mut m = MotionMatcher::new(db(), cfg, Vec3::ZERO, 0.0);
            let mut most = 0.0f32;
            let mut was = [None::<f32>; 2];
            for i in 0..600 {
                let a = i as f32 / 60.0 * 0.6;
                let s = ConstantSteering {
                    velocity: Vec3::new(a.sin(), 0.0, a.cos()) * 0.9,
                    facing: a,
                };
                m.step(1.0 / 60.0, &s);
                for (side, w) in was.iter_mut().enumerate() {
                    let h = foot_heading(&m, side);
                    if m.feet_held()[side] {
                        if let Some(h0) = *w {
                            most = most.max(wrap_angle(h - h0).abs());
                        }
                        *w = Some(h);
                    } else {
                        *w = None;
                    }
                }
            }
            most
        };
        let (kept, free) = (turned(true), turned(false));
        assert!(kept < 0.002, "a held foot turned {kept} rad in a step");
        assert!(
            free > 3.0 * kept.max(0.002),
            "without it the feet turned only {free} rad"
        );
    }

    #[test]
    fn a_foot_held_until_it_lifts_is_held_longer_and_stays_put() {
        // With hold_until_lift a planted point is let go only once it has risen: held on frames the
        // capture's contact has ended, and not moving over the floor on any of them.
        let run = |lift: f32| {
            let cfg = Config {
                hold_until_lift: lift,
                ..Config::default()
            };
            let mut m = MotionMatcher::new(db(), cfg, Vec3::ZERO, 0.0);
            let s = ConstantSteering {
                velocity: Vec3::new(0.0, 0.0, 1.0),
                facing: 0.0,
            };
            let legs = m.db.humanoid.legs;
            let (mut held, mut moved) = (0, 0.0f32);
            let mut was: [Option<(Vec3, Vec3)>; 2] = [None, None];
            for _ in 0..600 {
                m.step(1.0 / 60.0, &s);
                for side in 0..2 {
                    let w = m.world_positions();
                    let now = (w[legs[side].ankle], w[legs[side].toe]);
                    if m.feet_held()[side] {
                        held += 1;
                        if let Some((a, t)) = was[side] {
                            // The held point is the ankle or the toe: one of them stays put.
                            moved = moved.max(
                                ((now.0 - a).flat().length()).min((now.1 - t).flat().length()),
                            );
                        }
                        was[side] = Some(now);
                    } else {
                        was[side] = None;
                    }
                }
            }
            (held, moved)
        };
        let (off, moved_off) = run(0.0);
        let (on, moved) = run(0.02);
        assert!(on > off, "held {on} frames with it, {off} without");
        // (No more than a held point moves without it: the soft reach of a leg near straight.)
        assert!(
            moved <= moved_off + 1e-4,
            "a held point moved {moved} m in a step ({moved_off} without)"
        );
    }

    #[test]
    fn planted_feet_stay_where_they_were_put() {
        let mut m = MotionMatcher::new(db(), Config::default(), Vec3::ZERO, 0.0);
        let s = ConstantSteering {
            velocity: Vec3::ZERO,
            facing: 0.0,
        };
        for _ in 0..120 {
            m.step(1.0 / 60.0, &s);
        }
        // Handed over mid-scene: the feet where other animation left them, 4 cm off the pose's.
        let legs = m.db.humanoid.legs;
        let want = [0, 1].map(|i| m.world_positions()[legs[i].ankle] + Vec3::new(0.04, 0.0, -0.03));
        m.plant_feet([Some(want[0]), Some(want[1])]);
        m.step(1.0 / 60.0, &s);
        for (i, w) in want.iter().enumerate() {
            let at = m.world_positions()[legs[i].ankle];
            assert!(
                (at - *w).flat().length() < 0.01,
                "foot {i} at {at:?}, planted at {w:?}"
            );
        }
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
