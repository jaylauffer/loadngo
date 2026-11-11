//! The humanoid bones the pipeline and runtime need by role, found by their Mixamo-style
//! names (`Hips`, `LeftUpLeg`, `LeftLeg`, `LeftFoot`, `LeftToeBase`, ...). A `prefix:` such
//! as `mixamorig:` is ignored.

use crate::skeleton::Skeleton;

/// The name without any `prefix:` (Mixamo exports name bones `mixamorig:Hips`).
#[must_use]
pub fn base_name(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// Finds a bone by its Mixamo-style base name.
#[must_use]
pub fn find_bone(skeleton: &Skeleton, name: &str) -> Option<usize> {
    skeleton
        .names
        .iter()
        .position(|n| base_name(n).eq_ignore_ascii_case(name))
}

/// One leg's bones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Leg {
    pub up: usize,
    pub knee: usize,
    pub ankle: usize,
    pub toe: usize,
}

/// The bones the motion database and its runtime work with by role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Humanoid {
    pub hips: usize,
    pub legs: [Leg; 2],
    /// Upper arms (`LeftArm`, `RightArm`), for the body's facing.
    pub arms: [usize; 2],
}

/// A bone the humanoid needs is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingBone(pub &'static str);

impl std::fmt::Display for MissingBone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the skeleton has no bone named {:?}", self.0)
    }
}

impl std::error::Error for MissingBone {}

impl Humanoid {
    /// Finds every role by name.
    ///
    /// # Errors
    /// [`MissingBone`] naming the first role with no bone.
    pub fn find(skeleton: &Skeleton) -> Result<Self, MissingBone> {
        let f = |n: &'static str| find_bone(skeleton, n).ok_or(MissingBone(n));
        let leg = |side: &str| -> Result<Leg, MissingBone> {
            let (up, knee, ankle, toe) = if side == "Left" {
                ("LeftUpLeg", "LeftLeg", "LeftFoot", "LeftToeBase")
            } else {
                ("RightUpLeg", "RightLeg", "RightFoot", "RightToeBase")
            };
            Ok(Leg {
                up: f(up)?,
                knee: f(knee)?,
                ankle: f(ankle)?,
                toe: f(toe)?,
            })
        };
        Ok(Self {
            hips: f("Hips")?,
            legs: [leg("Left")?, leg("Right")?],
            arms: [f("LeftArm")?, f("RightArm")?],
        })
    }
}

/// The bones motion matching drives on a Mixamo-named humanoid: the body, without fingers
/// or face. A target bone outside this list keeps its rest pose relative to its parent.
pub const BODY_BONES: [&str; 22] = [
    "Hips",
    "Spine",
    "Spine1",
    "Spine2",
    "Neck",
    "Head",
    "LeftShoulder",
    "LeftArm",
    "LeftForeArm",
    "LeftHand",
    "RightShoulder",
    "RightArm",
    "RightForeArm",
    "RightHand",
    "LeftUpLeg",
    "LeftLeg",
    "LeftFoot",
    "LeftToeBase",
    "RightUpLeg",
    "RightLeg",
    "RightFoot",
    "RightToeBase",
];

/// The mirror-image bone name (`Left...` ↔ `Right...`), or the name itself.
#[must_use]
pub fn mirror_name(name: &str) -> String {
    let (prefix, base) = match name.rfind(':') {
        Some(i) => (&name[..=i], &name[i + 1..]),
        None => ("", name),
    };
    if let Some(rest) = base.strip_prefix("Left") {
        format!("{prefix}Right{rest}")
    } else if let Some(rest) = base.strip_prefix("Right") {
        format!("{prefix}Left{rest}")
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_names_swap_sides_and_keep_prefixes() {
        assert_eq!(mirror_name("LeftFoot"), "RightFoot");
        assert_eq!(mirror_name("mixamorig:RightHand"), "mixamorig:LeftHand");
        assert_eq!(mirror_name("Spine1"), "Spine1");
        assert_eq!(base_name("mixamorig:Hips"), "Hips");
    }
}
