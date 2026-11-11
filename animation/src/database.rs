//! The motion database: every frame of retargeted capture, with its root trajectory, foot
//! contacts and matching features, and its compact binary form.
//!
//! **Root.** Each frame has a root (the "simulation bone") on the ground: the hips'
//! projection and the body's facing, both smoothed over a fraction of a second, so the
//! side-to-side sway of the hips stays in the pose and the root moves smoothly.
//!
//! **Pose.** The hips' transform relative to the root, and every other driven bone's
//! rotation relative to its parent. Bone lengths are the target skeleton's rest offsets.
//!
//! **Features** (27 numbers a frame, in the root's frame): both ankles' positions and
//! velocities, the hips' velocity, and the root's future position and facing direction
//! 20, 40 and 60 frames ahead (⅓, ⅔ and 1 s at 60 fps). Each group is normalised by its
//! mean and its spread (one spread for the whole group, so its axes keep their relative
//! sizes) and weighted.
//!
//! **Contacts.** Heel (ankle) and ball (toe joint) of each foot, labelled where the joint
//! is near the floor and nearly still, cleaned of flickers.

use crate::humanoid::{base_name, find_bone, mirror_name, Humanoid, BODY_BONES};
use crate::math::{wrap_angle, Quat, Transform, Vec3};
use crate::skeleton::{forward_kinematics, Skeleton};

/// Numbers in a frame's feature vector.
pub const FEATURES: usize = 27;
/// Frames ahead of each trajectory sample.
pub const HORIZONS: [usize; 3] = [20, 40, 60];

/// Feature groups: `(first, end)` index ranges.
pub const GROUPS: [(usize, usize); 5] = [(0, 6), (6, 12), (12, 15), (15, 21), (21, 27)];

/// Contact bits.
pub const LEFT_HEEL: u8 = 1;
pub const LEFT_BALL: u8 = 2;
pub const RIGHT_HEEL: u8 = 4;
pub const RIGHT_BALL: u8 = 8;

const MAGIC: &[u8; 8] = b"LDGOANIM";
/// The binary format's version. Readers refuse any other.
pub const FORMAT_VERSION: u32 = 1;

/// How a database is built.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BuildConfig {
    /// Frames a second of every clip.
    pub fps: f32,
    /// Weights of the feature groups: foot positions, foot velocities, hip velocity,
    /// trajectory positions, trajectory directions.
    pub weights: [f32; 5],
    /// Gaussian smoothing of the root's position and facing (seconds).
    pub root_position_sigma: f32,
    pub root_facing_sigma: f32,
    /// Contact: the joint within this height of its rest height (m) ...
    pub contact_height: f32,
    /// ... and moving slower than this over the ground (m/s).
    pub contact_speed: f32,
    /// Add each clip's mirror image (left and right swapped).
    pub mirror: bool,
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            fps: 60.0,
            weights: [1.5, 1.0, 1.0, 1.0, 1.5],
            root_position_sigma: 0.3,
            root_facing_sigma: 0.25,
            contact_height: 0.045,
            contact_speed: 0.3,
            mirror: true,
        }
    }
}

/// One clip of retargeted motion: the target skeleton's local transforms, frame after
/// frame, at [`BuildConfig::fps`].
#[derive(Debug, Clone)]
pub struct SourceClip {
    pub name: String,
    /// `frame_count * target.len()` local transforms.
    pub local: Vec<Transform>,
}

/// A run of frames from one clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipRange {
    pub name: String,
    pub start: usize,
    pub end: usize,
    pub mirrored: bool,
}

/// Why a database could not be built or read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseError(pub String);

impl std::fmt::Display for DatabaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "motion database: {}", self.0)
    }
}

impl std::error::Error for DatabaseError {}

fn dberr<T>(m: impl Into<String>) -> Result<T, DatabaseError> {
    Err(DatabaseError(m.into()))
}

/// Axis-aligned bounds of feature vectors over runs of frames, for pruning the search.
#[derive(Debug, Clone, Default)]
pub(crate) struct Bounds {
    pub small_min: Vec<f32>,
    pub small_max: Vec<f32>,
    pub large_min: Vec<f32>,
    pub large_max: Vec<f32>,
    /// Per frame: the end of its clip.
    pub clip_end: Vec<u32>,
}

pub(crate) const SMALL_BOX: usize = 16;
pub(crate) const LARGE_BOX: usize = 64;

/// A motion database ready to search and play.
#[derive(Debug, Clone)]
pub struct Database {
    /// The driven bones only (`BODY_BONES` found in the target), the hips first. Rest
    /// transforms are relative to each bone's parent here.
    pub skeleton: Skeleton,
    /// The target hips' parent's world transform (an armature node), for turning the hips
    /// into that node's space.
    pub armature: Transform,
    pub humanoid: Humanoid,
    /// Each driven bone's index in the target skeleton it was built for, by name.
    pub fps: f32,
    pub clips: Vec<ClipRange>,
    /// Per frame: the root on the ground in its clip's space.
    pub root_pos: Vec<Vec3>,
    pub root_rot: Vec<Quat>,
    /// Per frame: the hips' position in the root's space.
    pub hips_pos: Vec<Vec3>,
    /// Per frame, per bone: the hips' rotation in the root's space, then each other bone's
    /// rotation relative to its parent.
    pub rotations: Vec<Quat>,
    /// Per frame: [`LEFT_HEEL`] | [`LEFT_BALL`] | [`RIGHT_HEEL`] | [`RIGHT_BALL`].
    pub contacts: Vec<u8>,
    /// Per frame: [`FEATURES`] normalised numbers.
    pub features: Vec<f32>,
    pub feature_mean: [f32; FEATURES],
    pub feature_scale: [f32; FEATURES],
    pub weights: [f32; 5],
    /// Rest heights over the floor of the ankle and of the toe joint.
    pub foot_heights: [f32; 2],
    pub(crate) bounds: Bounds,
}

/// Gaussian smoothing with edge clamping.
fn smooth(values: &[f32], sigma_frames: f32) -> Vec<f32> {
    let n = values.len();
    if n == 0 || sigma_frames <= 0.0 {
        return values.to_vec();
    }
    let radius = (sigma_frames * 3.0).ceil() as isize;
    let kernel: Vec<f32> = (-radius..=radius)
        .map(|k| (-(k as f32).powi(2) / (2.0 * sigma_frames * sigma_frames)).exp())
        .collect();
    let total: f32 = kernel.iter().sum();
    (0..n as isize)
        .map(|i| {
            let mut acc = 0.0;
            for (j, w) in kernel.iter().enumerate() {
                let k = (i + j as isize - radius).clamp(0, n as isize - 1) as usize;
                acc += values[k] * w;
            }
            acc / total
        })
        .collect()
}

/// Turns runs of `true` shorter than `min_on` off and gaps shorter than `min_gap` on.
fn clean_runs(flags: &mut [bool], min_on: usize, min_gap: usize) {
    let n = flags.len();
    // Fill short gaps between contacts.
    let mut i = 0;
    while i < n {
        if !flags[i] {
            let start = i;
            while i < n && !flags[i] {
                i += 1;
            }
            if start > 0 && i < n && i - start < min_gap {
                flags[start..i].fill(true);
            }
        } else {
            i += 1;
        }
    }
    // Drop short contacts.
    let mut i = 0;
    while i < n {
        if flags[i] {
            let start = i;
            while i < n && flags[i] {
                i += 1;
            }
            if i - start < min_on {
                flags[start..i].fill(false);
            }
        } else {
            i += 1;
        }
    }
}

/// Indices of driven bones in the target and the parent of each within the driven set.
fn driven_bones(target: &Skeleton) -> (Vec<usize>, Vec<Option<usize>>) {
    let mut keep: Vec<usize> = Vec::new();
    for (i, n) in target.names.iter().enumerate() {
        if BODY_BONES.contains(&base_name(n)) {
            keep.push(i);
        }
    }
    let parents = keep
        .iter()
        .map(|&b| {
            let mut p = target.parents[b];
            while let Some(pp) = p {
                if let Some(k) = keep.iter().position(|&x| x == pp) {
                    return Some(k);
                }
                p = target.parents[pp];
            }
            None
        })
        .collect();
    (keep, parents)
}

/// The mirror image of a clip of the target skeleton's local transforms.
///
/// # Errors
/// When the target has no hips.
pub fn mirror_clip(
    target: &Skeleton,
    local: &[Transform],
) -> Result<Vec<Transform>, DatabaseError> {
    let n = target.len();
    let hips = find_bone(target, "Hips").ok_or_else(|| DatabaseError("no Hips".into()))?;
    let rest_world = target.rest_world();
    let partner: Vec<usize> = target
        .names
        .iter()
        .enumerate()
        .map(|(i, name)| target.find(&mirror_name(name)).unwrap_or(i))
        .collect();
    let hips_parent = target.parents[hips].map_or(Transform::IDENTITY, |p| rest_world[p]);
    let frames = local.len() / n;
    let mut out = Vec::with_capacity(local.len());
    let mut world = vec![Transform::IDENTITY; n];
    let mut mrot = vec![Quat::IDENTITY; n];
    for f in 0..frames {
        let l = &local[f * n..(f + 1) * n];
        forward_kinematics(&target.parents, l, Transform::IDENTITY, &mut world);
        for b in 0..n {
            let p = partner[b];
            let delta = world[p].rotation * rest_world[p].rotation.inverse();
            mrot[b] = (delta.mirror_x() * rest_world[b].rotation).normalize();
        }
        for b in 0..n {
            let parent_rot = target.parents[b].map_or(Quat::IDENTITY, |p| mrot[p]);
            let mut t = target.rest[b].translation;
            if b == hips {
                let hw = world[hips].translation;
                t = hips_parent.inverse_apply(Vec3::new(-hw.x, hw.y, hw.z));
            }
            out.push(Transform::new(
                t,
                (parent_rot.inverse() * mrot[b]).normalize(),
            ));
        }
    }
    Ok(out)
}

impl Database {
    /// Builds a database for `target` from retargeted clips.
    ///
    /// # Errors
    /// [`DatabaseError`] for a target without the humanoid bones, a clip whose length is
    /// not a whole number of frames, or no frames at all.
    pub fn build(
        target: &Skeleton,
        clips: &[SourceClip],
        config: &BuildConfig,
    ) -> Result<Self, DatabaseError> {
        let n = target.len();
        let target_humanoid = Humanoid::find(target).map_err(|e| DatabaseError(e.to_string()))?;
        let (keep, kparents) = driven_bones(target);
        let rest_world = target.rest_world();
        let hips_t = target_humanoid.hips;
        if keep.first() != Some(&hips_t) {
            return dberr("the hips must be the first driven bone");
        }
        let names: Vec<String> = keep.iter().map(|&b| target.names[b].clone()).collect();
        let rest: Vec<Transform> = keep
            .iter()
            .zip(&kparents)
            .map(|(&b, p)| match p {
                Some(p) => rest_world[keep[*p]].inverse().compose(rest_world[b]),
                None => rest_world[b],
            })
            .collect();
        let skeleton = Skeleton::new(names, kparents.clone(), rest)
            .map_err(|e| DatabaseError(e.to_string()))?;
        let humanoid = Humanoid::find(&skeleton).map_err(|e| DatabaseError(e.to_string()))?;
        let armature = target.parents[hips_t].map_or(Transform::IDENTITY, |p| rest_world[p]);
        let foot_heights = [
            rest_world[target_humanoid.legs[0].ankle].translation.y,
            rest_world[target_humanoid.legs[0].toe].translation.y,
        ];

        let mut all: Vec<(String, bool, &[Transform])> = Vec::new();
        let mut mirrored_store: Vec<Vec<Transform>> = Vec::new();
        for c in clips {
            if c.local.len() % n != 0 || c.local.is_empty() {
                return dberr(format!("clip {:?} is not a whole number of frames", c.name));
            }
            if config.mirror {
                mirrored_store.push(mirror_clip(target, &c.local)?);
            }
        }
        for (i, c) in clips.iter().enumerate() {
            all.push((c.name.clone(), false, &c.local));
            if config.mirror {
                all.push((c.name.clone(), true, &mirrored_store[i]));
            }
        }

        let kb = keep.len();
        let mut db = Self {
            skeleton,
            armature,
            humanoid,
            fps: config.fps,
            clips: Vec::new(),
            root_pos: Vec::new(),
            root_rot: Vec::new(),
            hips_pos: Vec::new(),
            rotations: Vec::new(),
            contacts: Vec::new(),
            features: Vec::new(),
            feature_mean: [0.0; FEATURES],
            feature_scale: [1.0; FEATURES],
            weights: config.weights,
            foot_heights,
            bounds: Bounds::default(),
        };
        let dt = 1.0 / config.fps;
        let mut raw: Vec<f32> = Vec::new();
        let mut world = vec![Transform::IDENTITY; n];
        for (name, mirrored, local) in all {
            let frames = local.len() / n;
            let start = db.root_pos.len();
            // World transforms of the joints we need, per frame.
            let mut hips_w = Vec::with_capacity(frames);
            let mut feet = vec![[Vec3::ZERO; 4]; frames]; // l ankle, l toe, r ankle, r toe
            let mut lateral = Vec::with_capacity(frames);
            let mut locals = Vec::with_capacity(frames * kb);
            for f in 0..frames {
                let l = &local[f * n..(f + 1) * n];
                forward_kinematics(&target.parents, l, Transform::IDENTITY, &mut world);
                let h = world[hips_t];
                hips_w.push(h);
                let lg = &target_humanoid.legs;
                feet[f] = [
                    world[lg[0].ankle].translation,
                    world[lg[0].toe].translation,
                    world[lg[1].ankle].translation,
                    world[lg[1].toe].translation,
                ];
                let lat_h = world[lg[0].up].translation - world[lg[1].up].translation;
                let a = &target_humanoid.arms;
                let lat_s = world[a[0]].translation - world[a[1]].translation;
                lateral.push(lat_h.flat().normalize_or_zero() + lat_s.flat().normalize_or_zero());
                for (k, &b) in keep.iter().enumerate() {
                    let wb = world[b];
                    let lb = match kparents[k] {
                        Some(p) => world[keep[p]].inverse().compose(wb),
                        None => wb,
                    };
                    locals.push(lb);
                }
            }
            // Floor: the feet's lowest clearance over their rest heights, low percentile.
            let mut clear: Vec<f32> = feet
                .iter()
                .map(|ft| {
                    (ft[0].y - foot_heights[0])
                        .min(ft[1].y - foot_heights[1])
                        .min(ft[2].y - foot_heights[0])
                        .min(ft[3].y - foot_heights[1])
                })
                .collect();
            let floor = {
                let mut s = clear.clone();
                s.sort_by(f32::total_cmp);
                s[(s.len() as f32 * 0.1) as usize]
            };
            for c in &mut clear {
                *c -= floor;
            }
            for f in 0..frames {
                hips_w[f].translation.y -= floor;
                for p in &mut feet[f] {
                    p.y -= floor;
                }
                locals[f * kb].translation.y -= floor;
            }
            // Root: smoothed hips projection and facing.
            let xs: Vec<f32> = hips_w.iter().map(|h| h.translation.x).collect();
            let zs: Vec<f32> = hips_w.iter().map(|h| h.translation.z).collect();
            let sp = config.root_position_sigma * config.fps;
            let (xs, zs) = (smooth(&xs, sp), smooth(&zs, sp));
            let mut yaw: Vec<f32> = Vec::with_capacity(frames);
            let mut prev = 0.0f32;
            for (f, lat) in lateral.iter().enumerate() {
                let facing = lat.cross(Vec3::Y);
                let y = if facing.length_squared() > 1e-8 {
                    facing.x.atan2(facing.z)
                } else {
                    prev
                };
                // Unwrap.
                let y = if f == 0 {
                    y
                } else {
                    prev + wrap_angle(y - prev)
                };
                yaw.push(y);
                prev = y;
            }
            let yaw = smooth(&yaw, config.root_facing_sigma * config.fps);
            let roots: Vec<Transform> = (0..frames)
                .map(|f| Transform::new(Vec3::new(xs[f], 0.0, zs[f]), Quat::from_yaw(yaw[f])))
                .collect();
            // Contacts.
            let mut flags = [
                vec![false; frames],
                vec![false; frames],
                vec![false; frames],
                vec![false; frames],
            ];
            for (j, fl) in flags.iter_mut().enumerate() {
                let rest_h = foot_heights[j % 2];
                for f in 0..frames {
                    let a = feet[f.saturating_sub(1)][j];
                    let b = feet[(f + 1).min(frames - 1)][j];
                    let span = ((f + 1).min(frames - 1) - f.saturating_sub(1)).max(1) as f32;
                    let speed = (b - a).flat().length() / (span * dt);
                    fl[f] = feet[f][j].y - rest_h < config.contact_height
                        && speed < config.contact_speed;
                }
                clean_runs(fl, 5, 4);
            }
            // Store.
            for f in 0..frames {
                let root = roots[f];
                let hips_in_root = root.inverse().compose(hips_w[f]);
                db.root_pos.push(root.translation);
                db.root_rot.push(root.rotation);
                db.hips_pos.push(hips_in_root.translation);
                db.rotations.push(hips_in_root.rotation.normalize());
                for k in 1..kb {
                    db.rotations.push(locals[f * kb + k].rotation.normalize());
                }
                let c = (u8::from(flags[0][f]) * LEFT_HEEL)
                    | (u8::from(flags[1][f]) * LEFT_BALL)
                    | (u8::from(flags[2][f]) * RIGHT_HEEL)
                    | (u8::from(flags[3][f]) * RIGHT_BALL);
                db.contacts.push(c);
                // Features, raw.
                let fprev = f.saturating_sub(1);
                let fnext = (f + 1).min(frames - 1);
                let span = ((fnext - fprev).max(1)) as f32 * dt;
                let to_root = |p: Vec3| root.inverse_apply(p);
                let vel = |a: Vec3, b: Vec3| root.rotation.inverse_rotate((b - a) / span);
                let lp = to_root(feet[f][0]);
                let rp = to_root(feet[f][2]);
                let lv = vel(feet[fprev][0], feet[fnext][0]);
                let rv = vel(feet[fprev][2], feet[fnext][2]);
                let hv = vel(hips_w[fprev].translation, hips_w[fnext].translation);
                raw.extend_from_slice(&[lp.x, lp.y, lp.z, rp.x, rp.y, rp.z]);
                raw.extend_from_slice(&[lv.x, lv.y, lv.z, rv.x, rv.y, rv.z]);
                raw.extend_from_slice(&[hv.x, hv.y, hv.z]);
                for h in HORIZONS {
                    let t = roots[(f + h).min(frames - 1)];
                    let p = to_root(t.translation);
                    raw.extend_from_slice(&[p.x, p.z]);
                }
                for h in HORIZONS {
                    let t = roots[(f + h).min(frames - 1)];
                    let d = root.rotation.inverse_rotate(t.rotation.rotate(Vec3::Z));
                    raw.extend_from_slice(&[d.x, d.z]);
                }
            }
            db.clips.push(ClipRange {
                name,
                start,
                end: start + frames,
                mirrored,
            });
        }
        let total = db.root_pos.len();
        if total == 0 {
            return dberr("no frames");
        }
        // Normalise.
        let mut mean = [0.0f64; FEATURES];
        for f in 0..total {
            for (k, m) in mean.iter_mut().enumerate() {
                *m += f64::from(raw[f * FEATURES + k]);
            }
        }
        for m in &mut mean {
            *m /= total as f64;
        }
        let mut var = [0.0f64; FEATURES];
        for f in 0..total {
            for (k, v) in var.iter_mut().enumerate() {
                let d = f64::from(raw[f * FEATURES + k]) - mean[k];
                *v += d * d;
            }
        }
        for (g, &(a, b)) in GROUPS.iter().enumerate() {
            let mut std = 0.0f64;
            for v in &var[a..b] {
                std += (v / total as f64).sqrt();
            }
            std /= (b - a) as f64;
            let scale = (std.max(1e-6) / f64::from(config.weights[g].max(1e-6))) as f32;
            for (dst, src) in db.feature_mean[a..b].iter_mut().zip(&mean[a..b]) {
                *dst = *src as f32;
            }
            db.feature_scale[a..b].fill(scale);
        }
        db.features = raw;
        for f in 0..total {
            for k in 0..FEATURES {
                let v = &mut db.features[f * FEATURES + k];
                *v = (*v - db.feature_mean[k]) / db.feature_scale[k];
            }
        }
        db.build_bounds();
        Ok(db)
    }

    /// Frames in the database.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.root_pos.len()
    }

    /// Bones in each frame.
    #[must_use]
    pub fn bone_count(&self) -> usize {
        self.skeleton.len()
    }

    /// The clip holding `frame`.
    #[must_use]
    pub fn clip_of(&self, frame: usize) -> Option<&ClipRange> {
        self.clips
            .iter()
            .find(|c| frame >= c.start && frame < c.end)
    }

    /// The normalised feature vector of `frame`.
    #[must_use]
    pub fn feature(&self, frame: usize) -> &[f32] {
        &self.features[frame * FEATURES..(frame + 1) * FEATURES]
    }

    pub(crate) fn build_bounds(&mut self) {
        let total = self.frame_count();
        let ns = total.div_ceil(SMALL_BOX);
        let nl = total.div_ceil(LARGE_BOX);
        let mut b = Bounds {
            small_min: vec![f32::INFINITY; ns * FEATURES],
            small_max: vec![f32::NEG_INFINITY; ns * FEATURES],
            large_min: vec![f32::INFINITY; nl * FEATURES],
            large_max: vec![f32::NEG_INFINITY; nl * FEATURES],
            clip_end: vec![0; total],
        };
        for c in &self.clips {
            b.clip_end[c.start..c.end].fill(c.end as u32);
        }
        for f in 0..total {
            let s = f / SMALL_BOX;
            let l = f / LARGE_BOX;
            for k in 0..FEATURES {
                let v = self.features[f * FEATURES + k];
                b.small_min[s * FEATURES + k] = b.small_min[s * FEATURES + k].min(v);
                b.small_max[s * FEATURES + k] = b.small_max[s * FEATURES + k].max(v);
                b.large_min[l * FEATURES + k] = b.large_min[l * FEATURES + k].min(v);
                b.large_max[l * FEATURES + k] = b.large_max[l * FEATURES + k].max(v);
            }
        }
        self.bounds = b;
    }

    /// The compact binary form: header, skeleton, clips, normalisation, per-frame data
    /// (rotations as 16-bit fixed point) and a BLAKE3 hash of everything before it.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer(Vec::with_capacity(
            self.frame_count() * (self.bone_count() * 8 + FEATURES * 4 + 32),
        ));
        w.0.extend_from_slice(MAGIC);
        w.u32(FORMAT_VERSION);
        w.f32(self.fps);
        w.u32(self.bone_count() as u32);
        w.u32(self.frame_count() as u32);
        w.u32(self.clips.len() as u32);
        w.u32(FEATURES as u32);
        for i in 0..self.bone_count() {
            w.str(&self.skeleton.names[i]);
            w.i32(self.skeleton.parents[i].map_or(-1, |p| p as i32));
            w.transform(self.skeleton.rest[i]);
        }
        w.transform(self.armature);
        for v in self.foot_heights {
            w.f32(v);
        }
        for c in &self.clips {
            w.u32(c.start as u32);
            w.u32(c.end as u32);
            w.u32(u32::from(c.mirrored));
            w.str(&c.name);
        }
        for v in self
            .feature_mean
            .iter()
            .chain(&self.feature_scale)
            .chain(&self.weights)
        {
            w.f32(*v);
        }
        for f in 0..self.frame_count() {
            let p = self.root_pos[f];
            w.f32(p.x);
            w.f32(p.z);
            w.f32(self.root_rot[f].yaw());
            w.vec3(self.hips_pos[f]);
        }
        for q in &self.rotations {
            let q = q.abs();
            for c in [q.x, q.y, q.z, q.w] {
                w.0.extend_from_slice(
                    &((c.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes(),
                );
            }
        }
        w.0.extend_from_slice(&self.contacts);
        for v in &self.features {
            w.f32(*v);
        }
        let hash = blake3::hash(&w.0);
        w.0.extend_from_slice(hash.as_bytes());
        w.0
    }

    /// Reads [`Database::to_bytes`] output, checking its hash, version and sizes.
    ///
    /// # Errors
    /// [`DatabaseError`] for a wrong magic, version, hash or size, or inconsistent content.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DatabaseError> {
        if bytes.len() < MAGIC.len() + 4 + 32 || &bytes[..8] != MAGIC {
            return dberr("not a motion database (bad magic)");
        }
        let (body, hash) = bytes.split_at(bytes.len() - 32);
        if blake3::hash(body).as_bytes() != hash {
            return dberr("the hash does not match: truncated or damaged");
        }
        let mut r = Reader { b: body, at: 8 };
        let version = r.u32()?;
        if version != FORMAT_VERSION {
            return dberr(format!(
                "format version {version}, this reader knows {FORMAT_VERSION}"
            ));
        }
        let fps = r.f32()?;
        let bones = r.u32()? as usize;
        let frames = r.u32()? as usize;
        let nclips = r.u32()? as usize;
        if r.u32()? as usize != FEATURES {
            return dberr("unexpected feature count");
        }
        if bones == 0 || bones > 1024 || frames == 0 || fps.is_nan() || fps <= 0.0 {
            return dberr("implausible header");
        }
        let need = frames
            .checked_mul(bones * 8 + 1 + FEATURES * 4 + 24)
            .ok_or_else(|| DatabaseError("too large".into()))?;
        if need > body.len() {
            return dberr("shorter than its header says");
        }
        let mut names = Vec::with_capacity(bones);
        let mut parents = Vec::with_capacity(bones);
        let mut rest = Vec::with_capacity(bones);
        for _ in 0..bones {
            names.push(r.str()?);
            let p = r.i32()?;
            parents.push(if p < 0 { None } else { Some(p as usize) });
            rest.push(r.transform()?);
        }
        let skeleton =
            Skeleton::new(names, parents, rest).map_err(|e| DatabaseError(e.to_string()))?;
        let humanoid = Humanoid::find(&skeleton).map_err(|e| DatabaseError(e.to_string()))?;
        if humanoid.hips != 0 {
            return dberr("the hips must be bone 0");
        }
        let armature = r.transform()?;
        let foot_heights = [r.f32()?, r.f32()?];
        let mut clips = Vec::with_capacity(nclips.min(65536));
        for _ in 0..nclips {
            let start = r.u32()? as usize;
            let end = r.u32()? as usize;
            let mirrored = r.u32()? != 0;
            let name = r.str()?;
            if start >= end || end > frames {
                return dberr("a clip lies outside the frames");
            }
            clips.push(ClipRange {
                name,
                start,
                end,
                mirrored,
            });
        }
        let mut feature_mean = [0.0; FEATURES];
        let mut feature_scale = [0.0; FEATURES];
        let mut weights = [0.0; 5];
        for v in feature_mean
            .iter_mut()
            .chain(feature_scale.iter_mut())
            .chain(weights.iter_mut())
        {
            *v = r.f32()?;
        }
        let mut root_pos = Vec::with_capacity(frames);
        let mut root_rot = Vec::with_capacity(frames);
        let mut hips_pos = Vec::with_capacity(frames);
        for _ in 0..frames {
            let x = r.f32()?;
            let z = r.f32()?;
            let yaw = r.f32()?;
            root_pos.push(Vec3::new(x, 0.0, z));
            root_rot.push(Quat::from_yaw(yaw));
            hips_pos.push(r.vec3()?);
        }
        let mut rotations = Vec::with_capacity(frames * bones);
        for _ in 0..frames * bones {
            let mut c = [0.0f32; 4];
            for v in &mut c {
                *v = f32::from(r.i16()?) / 32767.0;
            }
            rotations.push(Quat::new(c[0], c[1], c[2], c[3]).normalize());
        }
        let contacts = r.take(frames)?.to_vec();
        let mut features = Vec::with_capacity(frames * FEATURES);
        for _ in 0..frames * FEATURES {
            features.push(r.f32()?);
        }
        if r.at != body.len() {
            return dberr("trailing bytes before the hash");
        }
        let mut db = Self {
            skeleton,
            armature,
            humanoid,
            fps,
            clips,
            root_pos,
            root_rot,
            hips_pos,
            rotations,
            contacts,
            features,
            feature_mean,
            feature_scale,
            weights,
            foot_heights,
            bounds: Bounds::default(),
        };
        db.build_bounds();
        Ok(db)
    }
}

struct Writer(Vec<u8>);

impl Writer {
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn vec3(&mut self, v: Vec3) {
        self.f32(v.x);
        self.f32(v.y);
        self.f32(v.z);
    }
    fn transform(&mut self, t: Transform) {
        self.vec3(t.translation);
        for c in [t.rotation.x, t.rotation.y, t.rotation.z, t.rotation.w] {
            self.f32(c);
        }
    }
    fn str(&mut self, s: &str) {
        let b = s.as_bytes();
        let len = b.len().min(u16::MAX as usize);
        self.0.extend_from_slice(&(len as u16).to_le_bytes());
        self.0.extend_from_slice(&b[..len]);
    }
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DatabaseError> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.b.len());
        match end {
            Some(e) => {
                let s = &self.b[self.at..e];
                self.at = e;
                Ok(s)
            }
            None => dberr("ends early"),
        }
    }
    fn u32(&mut self) -> Result<u32, DatabaseError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().unwrap_or_default(),
        ))
    }
    fn i32(&mut self) -> Result<i32, DatabaseError> {
        Ok(i32::from_le_bytes(
            self.take(4)?.try_into().unwrap_or_default(),
        ))
    }
    fn i16(&mut self) -> Result<i16, DatabaseError> {
        Ok(i16::from_le_bytes(
            self.take(2)?.try_into().unwrap_or_default(),
        ))
    }
    fn f32(&mut self) -> Result<f32, DatabaseError> {
        let v = f32::from_le_bytes(self.take(4)?.try_into().unwrap_or_default());
        if v.is_finite() {
            Ok(v)
        } else {
            dberr("a number is not finite")
        }
    }
    fn vec3(&mut self) -> Result<Vec3, DatabaseError> {
        Ok(Vec3::new(self.f32()?, self.f32()?, self.f32()?))
    }
    fn transform(&mut self) -> Result<Transform, DatabaseError> {
        let t = self.vec3()?;
        let q = Quat::new(self.f32()?, self.f32()?, self.f32()?, self.f32()?).normalize();
        Ok(Transform::new(t, q))
    }
    fn str(&mut self) -> Result<String, DatabaseError> {
        let len = u16::from_le_bytes(self.take(2)?.try_into().unwrap_or_default()) as usize;
        String::from_utf8(self.take(len)?.to_vec())
            .map_err(|_| DatabaseError("a name is not UTF-8".into()))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{humanoid_skeleton, synthetic_walk};

    #[test]
    fn smoothing_keeps_constants_and_damps_wiggles() {
        let c = smooth(&[2.0; 50], 5.0);
        assert!(c.iter().all(|v| (v - 2.0).abs() < 1e-5));
        let w: Vec<f32> = (0..200).map(|i| ((i as f32) * 0.5).sin()).collect();
        let s = smooth(&w, 10.0);
        assert!(s[100].abs() < 0.05);
    }

    #[test]
    fn runs_are_cleaned() {
        let mut f = [
            true, true, true, true, true, false, false, true, true, true, true, true, false, true,
            false, false,
        ];
        clean_runs(&mut f, 5, 4);
        assert_eq!(
            f,
            [
                true, true, true, true, true, true, true, true, true, true, true, true, true, true,
                false, false
            ]
        );
    }

    #[test]
    fn a_built_database_round_trips_through_bytes() {
        let target = humanoid_skeleton(1.0);
        let clip = synthetic_walk(&target, 240, 1.0);
        let db = Database::build(&target, &[clip], &BuildConfig::default()).unwrap();
        assert_eq!(db.clips.len(), 2);
        assert_eq!(db.frame_count(), 480);
        assert_eq!(db.bone_count(), 22);
        let bytes = db.to_bytes();
        let back = Database::from_bytes(&bytes).unwrap();
        assert_eq!(back.frame_count(), db.frame_count());
        assert_eq!(back.contacts, db.contacts);
        assert_eq!(back.features, db.features);
        for (a, b) in db.rotations.iter().zip(&back.rotations) {
            assert!(a.angle_to(*b) < 2e-4);
        }
        let mut bad = bytes.clone();
        bad[100] ^= 1;
        assert!(Database::from_bytes(&bad).is_err());
        assert!(Database::from_bytes(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn a_walk_has_alternating_contacts_and_forward_trajectory() {
        let target = humanoid_skeleton(1.0);
        let clip = synthetic_walk(&target, 300, 1.0);
        let cfg = BuildConfig {
            mirror: false,
            ..BuildConfig::default()
        };
        let db = Database::build(&target, &[clip], &cfg).unwrap();
        // Both feet touch down somewhere.
        assert!(db.contacts.iter().any(|c| c & LEFT_HEEL != 0));
        assert!(db.contacts.iter().any(|c| c & RIGHT_HEEL != 0));
        // Mid-clip the root heads +Z at about 1 m/s: 1/3 s ahead is ~0.33 m forward.
        let f = 150;
        let raw_z = db.features[f * FEATURES + 16] * db.feature_scale[16] + db.feature_mean[16];
        assert!((raw_z - 0.333).abs() < 0.05, "{raw_z}");
    }

    #[test]
    fn mirroring_swaps_the_feet() {
        let target = humanoid_skeleton(1.0);
        let clip = synthetic_walk(&target, 120, 1.0);
        let m = mirror_clip(&target, &clip.local).unwrap();
        let n = target.len();
        let mut w = vec![Transform::IDENTITY; n];
        let mut wm = vec![Transform::IDENTITY; n];
        let l = find_bone(&target, "LeftFoot").unwrap();
        let r = find_bone(&target, "RightFoot").unwrap();
        for f in [0, 37, 90] {
            forward_kinematics(
                &target.parents,
                &clip.local[f * n..(f + 1) * n],
                Transform::IDENTITY,
                &mut w,
            );
            forward_kinematics(
                &target.parents,
                &m[f * n..(f + 1) * n],
                Transform::IDENTITY,
                &mut wm,
            );
            let a = w[l].translation;
            let b = wm[r].translation;
            assert!((Vec3::new(-a.x, a.y, a.z) - b).length() < 1e-4, "frame {f}");
        }
    }
}
