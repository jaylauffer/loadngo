//! Retargeting motion from one skeleton to another by rest-pose alignment.
//!
//! Every mapped target bone takes its source bone's world-space change from rest:
//!
//! ```text
//! R_target(t) = D(t) * C * R_target_rest,   D(t) = R_source(t) * R_source_rest⁻¹
//! ```
//!
//! `C` aligns the two rest poses bone by bone: it turns the target bone's rest direction
//! (towards the child that both skeletons share) onto the source bone's, keeping a world
//! reference axis, so an A-posed target follows a T-posed source and a bone's own axis
//! convention never matters. Bone lengths are the target's own (its rest translations),
//! and the hips' travel is scaled by the ratio of leg lengths, so a shorter body takes
//! proportionally shorter steps with its feet still planted. A source whose rest faces
//! some other way is first turned to face +Z, like the target.
//!
//! An unmapped target bone (a finger, a face bone) keeps its rest rotation relative to its
//! parent.

use crate::humanoid::{base_name, find_bone};
use crate::math::{Quat, Transform, Vec3};
use crate::skeleton::{forward_kinematics, Skeleton};

/// Why two skeletons cannot be retargeted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetargetError(pub String);

impl std::fmt::Display for RetargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "retarget: {}", self.0)
    }
}

impl std::error::Error for RetargetError {}

/// Which source bone each target bone follows, by name: `(target, source)` pairs.
/// Target names are matched by their Mixamo base name (ignoring any `prefix:`).
#[derive(Debug, Clone, Default)]
pub struct BoneMap {
    pub pairs: Vec<(String, String)>,
}

impl BoneMap {
    /// Pairs every target bone with a source bone of the same base name.
    #[must_use]
    pub fn by_name(source: &Skeleton, target: &Skeleton) -> Self {
        let mut pairs = Vec::new();
        for t in &target.names {
            if let Some(s) = find_bone(source, base_name(t)) {
                pairs.push((base_name(t).to_string(), source.names[s].clone()));
            }
        }
        Self { pairs }
    }

    /// From `(target, source)` string pairs.
    #[must_use]
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Self {
            pairs: pairs
                .iter()
                .map(|(t, s)| ((*t).to_string(), (*s).to_string()))
                .collect(),
        }
    }
}

/// A prepared retargeting from one source skeleton to one target skeleton.
#[derive(Debug, Clone)]
pub struct Retargeter {
    target: Skeleton,
    /// Target bone → source bone.
    source_of: Vec<Option<usize>>,
    /// `C * R_target_rest` for each mapped target bone.
    post: Vec<Quat>,
    /// `(F * R_source_rest)⁻¹` for each source bone.
    source_rest_inv: Vec<Quat>,
    /// Turns the source's rest facing onto +Z.
    facing_fix: Quat,
    position_scale: f32,
    source_hips: usize,
    target_hips: usize,
    /// The target hips' parent's world transform (identity when the hips are a root).
    hips_parent_world: Transform,
}

/// The share of its neck's rest alignment a head with no end site of its own takes (see
/// `Retargeter::new`).
const HEAD_SHARE: f32 = 0.5;

fn basis(d: Vec3, sec: Vec3) -> Quat {
    let x = d.normalize_or_zero();
    let y = (sec - x * sec.dot(x)).normalize_or_zero();
    let z = x.cross(y);
    Quat::from_basis(x, y, z)
}

fn is_descendant(skeleton: &Skeleton, bone: usize, ancestor: usize) -> bool {
    let mut b = skeleton.parents[bone];
    while let Some(p) = b {
        if p == ancestor {
            return true;
        }
        b = skeleton.parents[p];
    }
    false
}

/// The facing of a rest pose from its hips: `(left - right) × up`, on the ground.
fn rest_facing(skeleton: &Skeleton, world: &[Transform]) -> Option<Vec3> {
    let l = find_bone(skeleton, "LeftUpLeg")?;
    let r = find_bone(skeleton, "RightUpLeg")?;
    let lateral = (world[l].translation - world[r].translation).flat();
    let f = lateral.cross(Vec3::Y).normalize_or_zero();
    (f.length_squared() > 0.5).then_some(f)
}

fn leg_length(
    skeleton: &Skeleton,
    world: &[Transform],
    up: usize,
    knee: usize,
    ankle: usize,
) -> f32 {
    let _ = skeleton;
    world[up].translation.distance(world[knee].translation)
        + world[knee].translation.distance(world[ankle].translation)
}

impl Retargeter {
    /// Prepares retargeting from `source` to `target` through `map`.
    ///
    /// The target's `Hips`, legs and feet must be mapped; the source rest pose must name
    /// its hips joints `LeftUpLeg`/`RightUpLeg` through the map or itself.
    ///
    /// # Errors
    /// [`RetargetError`] for an unknown bone in `map`, or an unmapped hips or leg.
    pub fn new(source: &Skeleton, target: &Skeleton, map: &BoneMap) -> Result<Self, RetargetError> {
        let mut source_of = vec![None; target.len()];
        for (t, s) in &map.pairs {
            let ti = find_bone(target, t)
                .ok_or_else(|| RetargetError(format!("the target has no bone {t:?}")))?;
            let si = source
                .find(s)
                .ok_or_else(|| RetargetError(format!("the source has no bone {s:?}")))?;
            source_of[ti] = Some(si);
        }
        let need = |name: &str| -> Result<(usize, usize), RetargetError> {
            let t = find_bone(target, name)
                .ok_or_else(|| RetargetError(format!("the target has no bone {name:?}")))?;
            let s = source_of[t]
                .ok_or_else(|| RetargetError(format!("the target's {name:?} is not mapped")))?;
            Ok((t, s))
        };
        let (target_hips, source_hips) = need("Hips")?;
        let (tlu, slu) = need("LeftUpLeg")?;
        let (tll, sll) = need("LeftLeg")?;
        let (tlf, slf) = need("LeftFoot")?;
        let (tru, sru) = need("RightUpLeg")?;
        need("RightLeg")?;
        need("RightFoot")?;

        let src_world = source.rest_world();
        let tgt_world = target.rest_world();

        // Face the source's rest pose towards +Z, as the target's is.
        let src_lateral = (src_world[slu].translation - src_world[sru].translation).flat();
        let src_facing = src_lateral.cross(Vec3::Y).normalize_or_zero();
        if src_facing.length_squared() < 0.5 {
            return Err(RetargetError(
                "the source's rest hips are not side by side".into(),
            ));
        }
        let facing_fix = Quat::from_yaw(-src_facing.x.atan2(src_facing.z));
        if let Some(tf) = rest_facing(target, &tgt_world) {
            if tf.dot(Vec3::Z) < 0.9 {
                return Err(RetargetError(format!(
                    "the target's rest pose faces ({:.2}, {:.2}), not +Z",
                    tf.x, tf.z
                )));
            }
        }
        let _ = tru;

        let src_len = leg_length(source, &src_world, slu, sll, slf);
        let tgt_len = leg_length(target, &tgt_world, tlu, tll, tlf);
        if src_len < 1e-4 || tgt_len < 1e-4 {
            return Err(RetargetError("a leg has no length at rest".into()));
        }

        let src_rot: Vec<Quat> = src_world.iter().map(|w| facing_fix * w.rotation).collect();
        let src_pos: Vec<Vec3> = src_world
            .iter()
            .map(|w| facing_fix.rotate(w.translation))
            .collect();

        // Rest alignment C for each mapped target bone.
        let mut align = vec![Quat::IDENTITY; target.len()];
        for b in 0..target.len() {
            let Some(s) = source_of[b] else {
                if let Some(p) = target.parents[b] {
                    align[b] = align[p];
                }
                continue;
            };
            let preferred = match base_name(&target.names[b]) {
                "Hips" => Some("Spine"),
                "Spine2" | "Spine1" | "Spine" => None,
                n if n.ends_with("Hand") => Some(if n.starts_with("Left") {
                    "LeftHandMiddle1"
                } else {
                    "RightHandMiddle1"
                }),
                _ => None,
            };
            let mut child_pair: Option<(usize, usize)> = None;
            for c in target.children(b) {
                let Some(sc) = source_of[c] else { continue };
                if !is_descendant(source, sc, s) {
                    continue;
                }
                let is_pref = preferred.is_some_and(|p| base_name(&target.names[c]) == p);
                if child_pair.is_none() || is_pref {
                    child_pair = Some((c, sc));
                }
                if is_pref {
                    break;
                }
            }
            // Leaves: pair a sole leaf child on each side (end sites).
            if child_pair.is_none() {
                let tc: Vec<usize> = target.children(b).collect();
                let sc: Vec<usize> = source.children(s).collect();
                if tc.len() == 1 && sc.len() == 1 && target.children(tc[0]).next().is_none() {
                    child_pair = Some((tc[0], sc[0]));
                }
            }
            let inherited = target.parents[b].map_or(Quat::IDENTITY, |p| align[p]);
            // A head with no end site of its own to pair (an avatar's: eyes beside its top) takes
            // half of the neck's alignment. All of it, as any unpaired bone, tipped the head of a
            // rig whose neck stands bent forward at rest back by the neck's straightening (21
            // degrees bent: about 11 degrees back, the face to the sky); none of it pitched every
            // head forward by the source's own head turn (4-13 degrees). Half put the head's top
            // within 2 degrees of a game's own animation on four bodies, bent necks and straight
            // (a browser game's round 7, measured on its records).
            if base_name(&target.names[b]) == "Head" && child_pair.is_none() {
                align[b] = Quat::IDENTITY.slerp(inherited, HEAD_SHARE);
                continue;
            }
            align[b] = match child_pair {
                Some((c, sc)) => {
                    let dt = tgt_world[c].translation - tgt_world[b].translation;
                    let ds = src_pos[sc] - src_pos[s];
                    if dt.length() < 1e-4 || ds.length() < 1e-4 {
                        inherited
                    } else {
                        let dn = dt.normalize_or_zero();
                        let sec = if dn.dot(Vec3::Z).abs() > 0.7 {
                            Vec3::Y
                        } else {
                            Vec3::Z
                        };
                        basis(ds, sec) * basis(dt, sec).inverse()
                    }
                }
                None => inherited,
            };
        }

        let post = (0..target.len())
            .map(|b| align[b] * tgt_world[b].rotation)
            .collect();
        let source_rest_inv = src_rot.iter().map(|q| q.inverse()).collect();
        let hips_parent_world =
            target.parents[target_hips].map_or(Transform::IDENTITY, |p| tgt_world[p]);
        Ok(Self {
            target: target.clone(),
            source_of,
            post,
            source_rest_inv,
            facing_fix,
            position_scale: tgt_len / src_len,
            source_hips,
            target_hips,
            hips_parent_world,
        })
    }

    /// How much the source's hips travel is scaled (target leg length over source's).
    #[must_use]
    pub fn position_scale(&self) -> f32 {
        self.position_scale
    }

    /// The target skeleton.
    #[must_use]
    pub fn target(&self) -> &Skeleton {
        &self.target
    }

    /// Retargets one frame. `source_local` holds the source skeleton's local transforms;
    /// `source_world` and `target_world_rot` are scratch as long as each skeleton; `out`
    /// receives the target's local transforms. The target's hips translation is in the
    /// source's world (turned to face +Z and scaled). Allocates nothing.
    pub fn frame(
        &self,
        source: &Skeleton,
        source_local: &[Transform],
        source_world: &mut [Transform],
        target_world_rot: &mut [Quat],
        out: &mut [Transform],
    ) {
        forward_kinematics(
            &source.parents,
            source_local,
            Transform::IDENTITY,
            source_world,
        );
        let t = &self.target;
        for b in 0..t.len() {
            let parent_rot = t.parents[b].map_or(Quat::IDENTITY, |p| target_world_rot[p]);
            let world = match self.source_of[b] {
                Some(s) => {
                    (self.facing_fix * source_world[s].rotation)
                        * self.source_rest_inv[s]
                        * self.post[b]
                }
                None => parent_rot * t.rest[b].rotation,
            };
            target_world_rot[b] = world.normalize();
            out[b] = Transform::new(
                t.rest[b].translation,
                (parent_rot.inverse() * world).normalize(),
            );
        }
        let hips_world = self
            .facing_fix
            .rotate(source_world[self.source_hips].translation)
            * self.position_scale;
        out[self.target_hips].translation = self.hips_parent_world.inverse_apply(hips_world);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal humanoid: hips, spine and two legs with feet and toes.
    pub(crate) fn legs_skeleton(leg: f32, arms_down: bool, facing_back: bool) -> Skeleton {
        let mut names = Vec::new();
        let mut parents = Vec::new();
        let mut rest = Vec::new();
        let flip = if facing_back { -1.0 } else { 1.0 };
        let mut add = |n: &str, p: Option<usize>, t: Vec3| {
            names.push(n.to_string());
            parents.push(p);
            rest.push(Transform::new(t, Quat::IDENTITY));
            names.len() - 1
        };
        let hips = add("Hips", None, Vec3::new(0.0, leg + 0.08, 0.0));
        let spine = add("Spine", Some(hips), Vec3::new(0.0, 0.1, 0.0));
        let _ = add("Spine1", Some(spine), Vec3::new(0.0, 0.1, 0.0));
        for (side, x) in [("Left", 0.1 * flip), ("Right", -0.1 * flip)] {
            let up = add(
                &format!("{side}UpLeg"),
                Some(hips),
                Vec3::new(x, -0.05, 0.0),
            );
            let knee = add(
                &format!("{side}Leg"),
                Some(up),
                Vec3::new(0.0, -leg / 2.0, 0.0),
            );
            let ankle = add(
                &format!("{side}Foot"),
                Some(knee),
                Vec3::new(0.0, -leg / 2.0, 0.0),
            );
            let _ = add(
                &format!("{side}ToeBase"),
                Some(ankle),
                Vec3::new(0.0, -0.05, 0.12 * flip),
            );
        }
        let _ = arms_down;
        Skeleton::new(names, parents, rest).unwrap()
    }

    #[test]
    fn a_head_on_a_bent_neck_takes_half_the_necks_alignment() {
        // Two targets alike but for the neck's rest: straight up, or bent 20 degrees forward
        // (the head turned back to level on it), each head with eyes beside its top. At the
        // source's rest pose the straight one's head stands as at its rest, and the bent one's
        // is tipped back by half of the 20 degrees its neck is straightened by, not all of it.
        let src = crate::testing::humanoid_skeleton(1.0);
        let bent = |deg: f32| {
            let t = crate::testing::humanoid_skeleton(1.0);
            let (neck, head) = (
                find_bone(&t, "Neck").unwrap(),
                find_bone(&t, "Head").unwrap(),
            );
            let (mut names, mut parents, mut rest) =
                (t.names.clone(), t.parents.clone(), t.rest.clone());
            rest[neck].rotation = Quat::from_axis_angle(Vec3::X, deg.to_radians());
            rest[head].rotation = Quat::from_axis_angle(Vec3::X, -deg.to_radians());
            for (n, x) in [("LeftEye", 0.03), ("RightEye", -0.03)] {
                names.push(n.to_string());
                parents.push(Some(head));
                rest.push(Transform::new(Vec3::new(x, 0.1, 0.08), Quat::IDENTITY));
            }
            Skeleton::new(names, parents, rest).unwrap()
        };
        let tipped = |t: &Skeleton| {
            // (The 22 driven bones only, as a capture's map gives them: no end site paired.)
            let mut map = BoneMap::by_name(&src, t);
            map.pairs.retain(|(n, _)| n != "HeadTop_End");
            let r = Retargeter::new(&src, t, &map).unwrap();
            let mut sw = vec![Transform::IDENTITY; src.len()];
            let mut tw = vec![Quat::IDENTITY; t.len()];
            let mut out = vec![Transform::IDENTITY; t.len()];
            r.frame(&src, &src.rest, &mut sw, &mut tw, &mut out);
            let head = find_bone(t, "Head").unwrap();
            t.rest_world()[head]
                .rotation
                .angle_to(tw[head])
                .to_degrees()
        };
        assert!(tipped(&bent(0.0)) < 0.01);
        let b = tipped(&bent(20.0));
        assert!((b - 10.0).abs() < 0.05, "tipped {b} degrees");
    }

    #[test]
    fn identical_skeletons_retarget_to_themselves() {
        let s = legs_skeleton(0.9, false, false);
        let r = Retargeter::new(&s, &s, &BoneMap::by_name(&s, &s)).unwrap();
        assert!((r.position_scale() - 1.0).abs() < 1e-6);
        let mut local = s.rest.clone();
        local[0].translation = Vec3::new(0.3, 0.95, 1.0);
        local[0].rotation = Quat::from_yaw(0.4);
        let knee = s.find("LeftLeg").unwrap();
        local[knee].rotation = Quat::from_axis_angle(Vec3::X, 0.6);
        let mut sw = vec![Transform::IDENTITY; s.len()];
        let mut tr = vec![Quat::IDENTITY; s.len()];
        let mut out = vec![Transform::IDENTITY; s.len()];
        r.frame(&s, &local, &mut sw, &mut tr, &mut out);
        for i in 0..s.len() {
            assert!(
                out[i].rotation.angle_to(local[i].rotation) < 1e-4,
                "bone {i}"
            );
            assert!(
                (out[i].translation - local[i].translation).length() < 1e-5,
                "bone {i}"
            );
        }
    }

    #[test]
    fn a_backwards_facing_longer_source_is_turned_and_scaled() {
        let src = legs_skeleton(1.0, false, true);
        let tgt = legs_skeleton(0.8, false, false);
        let r = Retargeter::new(&src, &tgt, &BoneMap::by_name(&src, &tgt)).unwrap();
        assert!((r.position_scale() - 0.8).abs() < 1e-5);
        // The source walks along its own facing (-Z) bending its left knee forwards.
        let mut local = src.rest.clone();
        local[0].translation = Vec3::new(0.0, 1.08, -2.0);
        let knee = src.find("LeftLeg").unwrap();
        local[knee].rotation = Quat::from_axis_angle(Vec3::X, -0.5); // shin back, for a -Z facer
        let mut sw = vec![Transform::IDENTITY; src.len()];
        let mut tr = vec![Quat::IDENTITY; tgt.len()];
        let mut out = vec![Transform::IDENTITY; tgt.len()];
        r.frame(&src, &local, &mut sw, &mut tr, &mut out);
        // Turned to face +Z: travel along +Z, scaled.
        assert!((out[0].translation - Vec3::new(0.0, 1.08 * 0.8, 1.6)).length() < 1e-4);
        // The target's shin goes back too (towards -Z for a +Z facer).
        let mut w = vec![Transform::IDENTITY; tgt.len()];
        forward_kinematics(&tgt.parents, &out, Transform::IDENTITY, &mut w);
        let tk = tgt.find("LeftLeg").unwrap();
        let ta = tgt.find("LeftFoot").unwrap();
        let shin = w[ta].translation - w[tk].translation;
        assert!(shin.z < -0.1, "shin {shin:?}");
    }
}
