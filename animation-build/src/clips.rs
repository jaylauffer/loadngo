//! Turning one capture file into a retargeted clip: parse, trim, resample, retarget. Runs
//! on a worker of the lane.

use loadngo_animation::asf;
use loadngo_animation::bvh;
use loadngo_animation::database::SourceClip;
use loadngo_animation::humanoid::find_bone;
use loadngo_animation::math::{Quat, Transform, Vec3};
use loadngo_animation::retarget::{BoneMap, Retargeter};
use loadngo_animation::skeleton::Skeleton;

/// 100STYLE (Xsens MVN skeleton) to Mixamo names.
pub const MAP_100STYLE: [(&str, &str); 22] = [
    ("Hips", "Hips"),
    ("Spine", "Chest"),
    ("Spine1", "Chest2"),
    ("Spine2", "Chest4"),
    ("Neck", "Neck"),
    ("Head", "Head"),
    ("LeftShoulder", "LeftCollar"),
    ("LeftArm", "LeftShoulder"),
    ("LeftForeArm", "LeftElbow"),
    ("LeftHand", "LeftWrist"),
    ("RightShoulder", "RightCollar"),
    ("RightArm", "RightShoulder"),
    ("RightForeArm", "RightElbow"),
    ("RightHand", "RightWrist"),
    ("LeftUpLeg", "LeftHip"),
    ("LeftLeg", "LeftKnee"),
    ("LeftFoot", "LeftAnkle"),
    ("LeftToeBase", "LeftToe"),
    ("RightUpLeg", "RightHip"),
    ("RightLeg", "RightKnee"),
    ("RightFoot", "RightAnkle"),
    ("RightToeBase", "RightToe"),
];

/// The CMU Graphics Lab database's BVH conversions to Mixamo names.
pub const MAP_CMU: [(&str, &str); 22] = [
    ("Hips", "Hips"),
    ("Spine", "LowerBack"),
    ("Spine1", "Spine"),
    ("Spine2", "Spine1"),
    ("Neck", "Neck"),
    ("Head", "Head"),
    ("LeftShoulder", "LeftShoulder"),
    ("LeftArm", "LeftArm"),
    ("LeftForeArm", "LeftForeArm"),
    ("LeftHand", "LeftHand"),
    ("RightShoulder", "RightShoulder"),
    ("RightArm", "RightArm"),
    ("RightForeArm", "RightForeArm"),
    ("RightHand", "RightHand"),
    ("LeftUpLeg", "LeftUpLeg"),
    ("LeftLeg", "LeftLeg"),
    ("LeftFoot", "LeftFoot"),
    ("LeftToeBase", "LeftToeBase"),
    ("RightUpLeg", "RightUpLeg"),
    ("RightLeg", "RightLeg"),
    ("RightFoot", "RightFoot"),
    ("RightToeBase", "RightToeBase"),
];

/// The CMU database's own ASF skeleton (`asf::parse_asf` joints) to Mixamo names.
pub const MAP_CMU_ASF: [(&str, &str); 22] = [
    ("Hips", "root"),
    ("Spine", "lowerback"),
    ("Spine1", "upperback"),
    ("Spine2", "thorax"),
    ("Neck", "lowerneck"),
    ("Head", "head"),
    ("LeftShoulder", "lclavicle"),
    ("LeftArm", "lhumerus"),
    ("LeftForeArm", "lradius"),
    ("LeftHand", "lwrist"),
    ("RightShoulder", "rclavicle"),
    ("RightArm", "rhumerus"),
    ("RightForeArm", "rradius"),
    ("RightHand", "rwrist"),
    ("LeftUpLeg", "lfemur"),
    ("LeftLeg", "ltibia"),
    ("LeftFoot", "lfoot"),
    ("LeftToeBase", "ltoes"),
    ("RightUpLeg", "rfemur"),
    ("RightLeg", "rtibia"),
    ("RightFoot", "rfoot"),
    ("RightToeBase", "rtoes"),
];

/// Mesh2Motion's motion-capture skeleton (its raw BVH captures) to Mixamo names.
pub const MAP_MESH2MOTION: [(&str, &str); 22] = [
    ("Hips", "root"),
    ("Spine", "torso_2"),
    ("Spine1", "torso_4"),
    ("Spine2", "torso_7"),
    ("Neck", "neck_1"),
    ("Head", "head"),
    ("LeftShoulder", "l_shoulder"),
    ("LeftArm", "l_up_arm"),
    ("LeftForeArm", "l_low_arm"),
    ("LeftHand", "l_hand"),
    ("RightShoulder", "r_shoulder"),
    ("RightArm", "r_up_arm"),
    ("RightForeArm", "r_low_arm"),
    ("RightHand", "r_hand"),
    ("LeftUpLeg", "l_up_leg"),
    ("LeftLeg", "l_low_leg"),
    ("LeftFoot", "l_foot"),
    ("LeftToeBase", "l_toes"),
    ("RightUpLeg", "r_up_leg"),
    ("RightLeg", "r_low_leg"),
    ("RightFoot", "r_foot"),
    ("RightToeBase", "r_toes"),
];

/// Which bone map a source skeleton needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapKind {
    Style100,
    Cmu,
    CmuAsf,
    Mesh2Motion,
    ByName,
}

impl MapKind {
    /// From a `--map` value.
    #[must_use]
    pub fn parse(s: &str) -> Option<Option<Self>> {
        match s {
            "auto" => Some(None),
            "100style" => Some(Some(Self::Style100)),
            "cmu" => Some(Some(Self::Cmu)),
            "cmu-asf" => Some(Some(Self::CmuAsf)),
            "mesh2motion" => Some(Some(Self::Mesh2Motion)),
            "name" => Some(Some(Self::ByName)),
            _ => None,
        }
    }

    /// Guesses from the joint names.
    #[must_use]
    pub fn detect(source: &Skeleton) -> Self {
        if source.find("Chest4").is_some() && source.find("LeftCollar").is_some() {
            Self::Style100
        } else if source.find("LowerBack").is_some() && source.find("LHipJoint").is_some() {
            Self::Cmu
        } else if source.find("lowerback").is_some() && source.find("lfemur").is_some() {
            Self::CmuAsf
        } else if source.find("torso_1").is_some() && source.find("l_up_leg").is_some() {
            Self::Mesh2Motion
        } else {
            Self::ByName
        }
    }

    /// The map for `source` and `target`, keeping only pairs both skeletons have.
    #[must_use]
    pub fn bone_map(self, source: &Skeleton, target: &Skeleton) -> BoneMap {
        let pairs: &[(&str, &str)] = match self {
            Self::Style100 => &MAP_100STYLE,
            Self::Cmu => &MAP_CMU,
            Self::CmuAsf => &MAP_CMU_ASF,
            Self::Mesh2Motion => &MAP_MESH2MOTION,
            Self::ByName => return BoneMap::by_name(source, target),
        };
        let kept: Vec<(&str, &str)> = pairs
            .iter()
            .copied()
            .filter(|(t, s)| find_bone(target, t).is_some() && source.find(s).is_some())
            .collect();
        BoneMap::from_pairs(&kept)
    }
}

/// How to turn a file into a clip.
#[derive(Debug, Clone)]
pub struct ClipJob {
    pub name: String,
    /// The motion: BVH text, or AMC text with `skeleton`.
    pub text: Vec<u8>,
    /// An ASF skeleton, for AMC motion.
    pub skeleton: Option<Vec<u8>>,
    /// The motion's frame rate where the file does not say (AMC; CMU's is 120).
    pub source_fps: Option<f32>,
    /// Seconds of the motion to keep, after `cut`.
    pub range: Option<(f32, f32)>,
    /// `database::tags` bits for the clip.
    pub tags: u32,
    /// Add the clip's mirror image.
    pub mirror: bool,
    /// Gaussian smoothing of every bone's rotation and the hips' travel (seconds; 0 keeps
    /// the capture as it is): takes out marker jitter a capture carries.
    pub smooth: f32,
    /// Source units to metres; `None` guesses from the leg length.
    pub scale: Option<f32>,
    pub map: Option<MapKind>,
    /// Frames to keep, `[start, end)` in source frames.
    pub cut: Option<(usize, usize)>,
    pub fps: f32,
    pub target: std::sync::Arc<Skeleton>,
}

/// A retargeted clip and what was learned on the way.
#[derive(Debug, Clone)]
pub struct ClipOut {
    pub clip: SourceClip,
    pub source_frames: usize,
    pub source_fps: f32,
    pub scale: f32,
    pub map: MapKind,
    pub mapped: usize,
    pub position_scale: f32,
}

/// Guesses source units from the rest leg length: over 10 units for a leg means
/// centimetres; over 100, millimetres.
fn guess_scale(s: &Skeleton) -> f32 {
    // The rest pose's height span: under 2.5 units for a person in metres, about 170 in
    // centimetres, about 1,700 in millimetres.
    let w = s.rest_world();
    let (lo, hi) = w
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), t| {
            (lo.min(t.translation.y), hi.max(t.translation.y))
        });
    let span = hi - lo;
    if span > 250.0 {
        0.001
    } else if span > 2.5 {
        0.01
    } else {
        1.0
    }
}

/// Parses, trims, resamples and retargets one capture.
///
/// # Errors
/// A parse or retarget failure, as text.
pub fn run(job: ClipJob) -> Result<ClipOut, String> {
    let text =
        std::str::from_utf8(&job.text).map_err(|_| format!("{}: not UTF-8 text", job.name))?;
    let source = match &job.skeleton {
        Some(sk) => {
            let sk = std::str::from_utf8(sk)
                .map_err(|_| format!("{}: the ASF is not UTF-8", job.name))?;
            let a = asf::parse_asf(sk).map_err(|e| format!("{}: {e}", job.name))?;
            let local = asf::parse_amc(text, &a).map_err(|e| format!("{}: {e}", job.name))?;
            let frame_count = local.len() / a.skeleton.len();
            Source {
                skeleton: a.skeleton,
                local,
                frame_count,
                fps: job.source_fps.unwrap_or(120.0),
                scale: a.scale,
            }
        }
        None => {
            let probe = bvh::parse(text, 1.0).map_err(|e| format!("{}: {e}", job.name))?;
            let scale = job.scale.unwrap_or_else(|| guess_scale(&probe.skeleton));
            let clip = if (scale - 1.0).abs() > f32::EPSILON {
                bvh::parse(text, scale).map_err(|e| format!("{}: {e}", job.name))?
            } else {
                probe
            };
            let n = clip.skeleton.len();
            let mut local = vec![Transform::IDENTITY; clip.frame_count * n];
            for f in 0..clip.frame_count {
                clip.frame_local(f, &mut local[f * n..(f + 1) * n]);
            }
            Source {
                fps: clip.fps(),
                frame_count: clip.frame_count,
                skeleton: clip.skeleton,
                local,
                scale,
            }
        }
    };
    run_source(&job, source)
}

/// A parsed capture: the source skeleton's local transforms frame after frame.
struct Source {
    skeleton: Skeleton,
    local: Vec<Transform>,
    frame_count: usize,
    fps: f32,
    scale: f32,
}

fn run_source(job: &ClipJob, clip: Source) -> Result<ClipOut, String> {
    let scale = clip.scale;
    let map = job.map.unwrap_or_else(|| MapKind::detect(&clip.skeleton));
    let target = &job.target;
    let bone_map = map.bone_map(&clip.skeleton, target);
    let retarget = Retargeter::new(&clip.skeleton, target, &bone_map)
        .map_err(|e| format!("{}: {e}", job.name))?;
    let (c0, c1) = job.cut.unwrap_or((0, clip.frame_count));
    let (mut c0, mut c1) = (c0.min(clip.frame_count), c1.min(clip.frame_count));
    if let Some((a, b)) = job.range {
        let (a, b) = (
            (a * clip.fps).round() as usize,
            (b * clip.fps).round() as usize,
        );
        c1 = (c0 + b).min(c1);
        c0 = (c0 + a).min(c1);
    }
    if c1 <= c0 + 2 {
        return Err(format!("{}: nothing left after the cut", job.name));
    }
    let src_n = clip.skeleton.len();
    let tgt_n = target.len();
    let mut src_local = vec![Transform::IDENTITY; src_n];
    let mut src_world = vec![Transform::IDENTITY; src_n];
    let mut tgt_world_rot = vec![Quat::IDENTITY; tgt_n];
    let mut frames: Vec<Vec<Transform>> = Vec::with_capacity(c1 - c0);
    for f in c0..c1 {
        let mut out = vec![Transform::IDENTITY; tgt_n];
        src_local.copy_from_slice(&clip.local[f * src_n..(f + 1) * src_n]);
        retarget.frame(
            &clip.skeleton,
            &src_local,
            &mut src_world,
            &mut tgt_world_rot,
            &mut out,
        );
        frames.push(out);
    }
    if job.smooth > 0.0 {
        smooth_frames(&mut frames, job.smooth * clip.fps);
    }
    // Resample to the database's rate.
    let src_fps = clip.fps;
    let duration = (frames.len() - 1) as f32 / src_fps;
    let out_n = (duration * job.fps).floor() as usize + 1;
    let mut local = Vec::with_capacity(out_n * tgt_n);
    for i in 0..out_n {
        let t = i as f32 / job.fps * src_fps;
        let a = (t.floor() as usize).min(frames.len() - 1);
        let b = (a + 1).min(frames.len() - 1);
        let u = t - a as f32;
        for (x, y) in frames[a].iter().zip(&frames[b]) {
            local.push(Transform::new(
                x.translation.lerp(y.translation, u),
                x.rotation.slerp(y.rotation, u),
            ));
        }
    }
    Ok(ClipOut {
        clip: SourceClip {
            name: job.name.clone(),
            tags: job.tags,
            mirror: job.mirror,
            local,
        },
        source_frames: clip.frame_count,
        source_fps: src_fps,
        scale,
        map,
        mapped: bone_map.pairs.len(),
        position_scale: retarget.position_scale(),
    })
}

/// Gaussian smoothing over frames of each bone's local rotation (sign-aligned, normalised)
/// and translation, with edges clamped.
fn smooth_frames(frames: &mut [Vec<Transform>], sigma: f32) {
    if frames.len() < 3 || sigma < 0.3 {
        return;
    }
    let radius = (sigma * 3.0).ceil() as isize;
    let w: Vec<f32> = (-radius..=radius)
        .map(|k| (-(k * k) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let n = frames.len() as isize;
    let bones = frames[0].len();
    let src: Vec<Vec<Transform>> = frames.to_vec();
    for f in 0..n {
        for b in 0..bones {
            let centre = src[f as usize][b].rotation;
            let (mut q, mut t, mut total) = ([0.0f32; 4], Vec3::ZERO, 0.0f32);
            for (j, wj) in w.iter().enumerate() {
                let k = (f + j as isize - radius).clamp(0, n - 1) as usize;
                let r = src[k][b].rotation.nearest(centre);
                q[0] += r.x * wj;
                q[1] += r.y * wj;
                q[2] += r.z * wj;
                q[3] += r.w * wj;
                t += src[k][b].translation * *wj;
                total += wj;
            }
            frames[f as usize][b] =
                Transform::new(t / total, Quat::new(q[0], q[1], q[2], q[3]).normalize());
        }
    }
}

/// Frame cuts from a 100STYLE-style `Frame_Cuts.csv`: `STYLE_NAME,BR_START,BR_STOP,...`.
/// Returns `(style, kind) -> (start, stop)`.
#[must_use]
pub fn frame_cuts(csv: &str) -> Vec<((String, String), (usize, usize))> {
    let mut lines = csv.lines();
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let cols: Vec<&str> = header.trim().split(',').collect();
    let mut out = Vec::new();
    for line in lines {
        let cells: Vec<&str> = line.trim().split(',').collect();
        let Some(style) = cells.first() else { continue };
        for (i, c) in cols.iter().enumerate() {
            if let Some(kind) = c.strip_suffix("_START") {
                let stop_col = format!("{kind}_STOP");
                let Some(j) = cols.iter().position(|x| *x == stop_col) else {
                    continue;
                };
                if let (Some(a), Some(b)) = (cells.get(i), cells.get(j)) {
                    if let (Ok(a), Ok(b)) = (a.parse::<usize>(), b.parse::<usize>()) {
                        out.push(((style.to_string(), kind.to_string()), (a, b)));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_cuts_are_read_by_style_and_kind() {
        let csv = "STYLE_NAME,BR_START,BR_STOP,FW_START,FW_STOP\nNeutral,338,5481,327,7445\nOdd,N/A,N/A,5,9\n";
        let c = frame_cuts(csv);
        assert!(c.contains(&(("Neutral".into(), "FW".into()), (327, 7445))));
        assert!(c.contains(&(("Odd".into(), "FW".into()), (5, 9))));
        assert!(!c.iter().any(|((s, k), _)| s == "Odd" && k == "BR"));
    }
}
