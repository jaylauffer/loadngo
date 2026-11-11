//! Turning one capture file into a retargeted clip: parse, trim, resample, retarget. Runs
//! on a worker of the lane.

use loadngo_animation::bvh;
use loadngo_animation::database::SourceClip;
use loadngo_animation::humanoid::find_bone;
use loadngo_animation::math::{Quat, Transform};
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

/// Which bone map a source skeleton needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapKind {
    Style100,
    Cmu,
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
    pub text: Vec<u8>,
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
    let w = s.rest_world();
    let leg = ["LeftHip", "LeftUpLeg"]
        .iter()
        .find_map(|n| s.find(n))
        .and_then(|up| {
            let ankle = ["LeftAnkle", "LeftFoot"].iter().find_map(|n| s.find(n))?;
            Some(w[up].translation.distance(w[ankle].translation))
        })
        .unwrap_or(1.0);
    if leg > 100.0 {
        0.001
    } else if leg > 10.0 {
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
    let probe = bvh::parse(text, 1.0).map_err(|e| format!("{}: {e}", job.name))?;
    let scale = job.scale.unwrap_or_else(|| guess_scale(&probe.skeleton));
    let clip = if (scale - 1.0).abs() > f32::EPSILON {
        bvh::parse(text, scale).map_err(|e| format!("{}: {e}", job.name))?
    } else {
        probe
    };
    let map = job.map.unwrap_or_else(|| MapKind::detect(&clip.skeleton));
    let target = &job.target;
    let bone_map = map.bone_map(&clip.skeleton, target);
    let retarget = Retargeter::new(&clip.skeleton, target, &bone_map)
        .map_err(|e| format!("{}: {e}", job.name))?;
    let (c0, c1) = job.cut.unwrap_or((0, clip.frame_count));
    let (c0, c1) = (c0.min(clip.frame_count), c1.min(clip.frame_count));
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
        clip.frame_local(f, &mut src_local);
        retarget.frame(
            &clip.skeleton,
            &src_local,
            &mut src_world,
            &mut tgt_world_rot,
            &mut out,
        );
        frames.push(out);
    }
    // Resample to the database's rate.
    let src_fps = clip.fps();
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
            name: job.name,
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
