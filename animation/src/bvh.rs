//! A BVH (Biovision Hierarchy) importer.
//!
//! Reads the `HIERARCHY` (joints, offsets, channels, end sites) and the `MOTION` block.
//! Rotation channels compose in the order listed, as BVH defines: `Zrotation Xrotation
//! Yrotation` is `Rz * Rx * Ry`. End sites become leaf bones named `<parent>_End`, which
//! keeps the length and direction of the last bone of each chain. Translation channels
//! may appear on any joint; a joint without them keeps its offset.

use crate::math::{Quat, Transform, Vec3};
use crate::skeleton::Skeleton;

/// One BVH channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Xpos,
    Ypos,
    Zpos,
    Xrot,
    Yrot,
    Zrot,
}

/// Where a joint's channels sit in each frame's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JointChannels {
    pub first: usize,
    pub channels: Vec<Channel>,
}

/// A parsed BVH file.
#[derive(Debug, Clone)]
pub struct BvhClip {
    /// The joints (and end sites) with their offsets as rest translations; rest rotations
    /// are identity, as BVH defines.
    pub skeleton: Skeleton,
    /// Channels of each bone of `skeleton` (end sites have none).
    pub joints: Vec<JointChannels>,
    pub frame_time: f32,
    pub frame_count: usize,
    pub channels_per_frame: usize,
    /// `frame_count * channels_per_frame` values: degrees for rotations, file units for
    /// translations (already multiplied by the scale given to [`parse`]).
    pub data: Vec<f32>,
}

/// Why a BVH file could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BvhError(pub String);

impl std::fmt::Display for BvhError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BVH: {}", self.0)
    }
}

impl std::error::Error for BvhError {}

fn err<T>(msg: impl Into<String>) -> Result<T, BvhError> {
    Err(BvhError(msg.into()))
}

struct Tokens<'a> {
    inner: std::str::SplitAsciiWhitespace<'a>,
}

impl<'a> Tokens<'a> {
    fn next(&mut self, what: &str) -> Result<&'a str, BvhError> {
        match self.inner.next() {
            Some(t) => Ok(t),
            None => err(format!("the file ended where {what} was expected")),
        }
    }

    fn expect(&mut self, word: &str) -> Result<(), BvhError> {
        let t = self.next(word)?;
        if t.eq_ignore_ascii_case(word) {
            Ok(())
        } else {
            err(format!("expected {word:?}, found {t:?}"))
        }
    }

    fn float(&mut self, what: &str) -> Result<f32, BvhError> {
        let t = self.next(what)?;
        match t.parse::<f32>() {
            Ok(v) if v.is_finite() => Ok(v),
            _ => err(format!("{what}: {t:?} is not a finite number")),
        }
    }
}

/// Parses BVH text. `scale` multiplies every offset and translation (0.01 turns
/// centimetres into metres).
///
/// # Errors
/// [`BvhError`] for malformed text, a non-finite value, or a frame count that does not
/// match the data.
pub fn parse(text: &str, scale: f32) -> Result<BvhClip, BvhError> {
    let mut tok = Tokens {
        inner: text.split_ascii_whitespace(),
    };
    tok.expect("HIERARCHY")?;
    let mut names: Vec<String> = Vec::new();
    let mut parents: Vec<Option<usize>> = Vec::new();
    let mut rest: Vec<Transform> = Vec::new();
    let mut joints: Vec<JointChannels> = Vec::new();
    let mut channel_total = 0usize;
    // Stack of open joints.
    let mut stack: Vec<usize> = Vec::new();

    let first = tok.next("ROOT")?;
    if !first.eq_ignore_ascii_case("ROOT") {
        return err(format!("expected ROOT, found {first:?}"));
    }
    let mut pending: Option<(String, bool)> =
        Some((tok.next("the root's name")?.to_string(), false));
    loop {
        if let Some((name, end_site)) = pending.take() {
            tok.expect("{")?;
            let parent = stack.last().copied();
            let index = names.len();
            let unique = if names.contains(&name) {
                format!("{name}_{index}")
            } else {
                name
            };
            names.push(unique);
            parents.push(parent);
            tok.expect("OFFSET")?;
            let o = Vec3::new(
                tok.float("an offset")?,
                tok.float("an offset")?,
                tok.float("an offset")?,
            ) * scale;
            rest.push(Transform::new(o, Quat::IDENTITY));
            let mut jc = JointChannels {
                first: channel_total,
                channels: Vec::new(),
            };
            if !end_site {
                tok.expect("CHANNELS")?;
                let n = tok
                    .next("a channel count")?
                    .parse::<usize>()
                    .map_err(|_| BvhError("bad channel count".into()))?;
                if n > 6 {
                    return err(format!("{n} channels on one joint"));
                }
                for _ in 0..n {
                    let c = tok.next("a channel name")?;
                    let ch = match c.to_ascii_lowercase().as_str() {
                        "xposition" => Channel::Xpos,
                        "yposition" => Channel::Ypos,
                        "zposition" => Channel::Zpos,
                        "xrotation" => Channel::Xrot,
                        "yrotation" => Channel::Yrot,
                        "zrotation" => Channel::Zrot,
                        other => return err(format!("unknown channel {other:?}")),
                    };
                    jc.channels.push(ch);
                }
                channel_total += n;
            }
            joints.push(jc);
            stack.push(index);
        }
        let t = tok.next("JOINT, End or }")?;
        if t == "}" {
            stack.pop();
            if stack.is_empty() {
                break;
            }
        } else if t.eq_ignore_ascii_case("JOINT") {
            pending = Some((tok.next("a joint name")?.to_string(), false));
        } else if t.eq_ignore_ascii_case("End") {
            tok.expect("Site")?;
            let parent = stack.last().copied().unwrap_or(0);
            pending = Some((format!("{}_End", names[parent]), true));
        } else {
            return err(format!("unexpected {t:?} in the hierarchy"));
        }
    }

    tok.expect("MOTION")?;
    tok.expect("Frames:")?;
    let frame_count = tok
        .next("the frame count")?
        .parse::<usize>()
        .map_err(|_| BvhError("bad frame count".into()))?;
    tok.expect("Frame")?;
    tok.expect("Time:")?;
    let frame_time = tok.float("the frame time")?;
    if frame_time <= 0.0 {
        return err("the frame time must be positive");
    }
    let total = frame_count
        .checked_mul(channel_total)
        .ok_or_else(|| BvhError("frame data too large".into()))?;
    let mut data = Vec::with_capacity(total);
    // Which values are translations (to scale).
    let mut is_pos = vec![false; channel_total];
    for j in &joints {
        for (k, c) in j.channels.iter().enumerate() {
            is_pos[j.first + k] = matches!(c, Channel::Xpos | Channel::Ypos | Channel::Zpos);
        }
    }
    for i in 0..total {
        let v = tok.float("frame data")?;
        data.push(if is_pos[i % channel_total.max(1)] {
            v * scale
        } else {
            v
        });
    }
    let skeleton =
        Skeleton::new(names, parents, rest).map_err(|e| BvhError(format!("the hierarchy: {e}")))?;
    Ok(BvhClip {
        skeleton,
        joints,
        frame_time,
        frame_count,
        channels_per_frame: channel_total,
        data,
    })
}

impl BvhClip {
    /// Frames per second.
    #[must_use]
    pub fn fps(&self) -> f32 {
        1.0 / self.frame_time
    }

    /// Local transforms of every bone at `frame`. `out` must be as long as the skeleton.
    pub fn frame_local(&self, frame: usize, out: &mut [Transform]) {
        let row =
            &self.data[frame * self.channels_per_frame..(frame + 1) * self.channels_per_frame];
        for (i, j) in self.joints.iter().enumerate() {
            let mut t = self.skeleton.rest[i].translation;
            let mut q = Quat::IDENTITY;
            let mut has_pos = [false; 3];
            let mut pos = [0.0f32; 3];
            for (k, c) in j.channels.iter().enumerate() {
                let v = row[j.first + k];
                match c {
                    Channel::Xpos => (has_pos[0], pos[0]) = (true, v),
                    Channel::Ypos => (has_pos[1], pos[1]) = (true, v),
                    Channel::Zpos => (has_pos[2], pos[2]) = (true, v),
                    Channel::Xrot => q = q * Quat::from_axis_angle(Vec3::X, v.to_radians()),
                    Channel::Yrot => q = q * Quat::from_axis_angle(Vec3::Y, v.to_radians()),
                    Channel::Zrot => q = q * Quat::from_axis_angle(Vec3::Z, v.to_radians()),
                }
            }
            if has_pos[0] {
                t.x = pos[0];
            }
            if has_pos[1] {
                t.y = pos[1];
            }
            if has_pos[2] {
                t.z = pos[2];
            }
            out[i] = Transform::new(t, q.normalize());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: &str = "HIERARCHY
ROOT Hips
{
  OFFSET 0 100 0
  CHANNELS 6 Xposition Yposition Zposition Zrotation Xrotation Yrotation
  JOINT Spine
  {
    OFFSET 0 10 0
    CHANNELS 3 Zrotation Xrotation Yrotation
    End Site
    {
      OFFSET 0 20 0
    }
  }
}
MOTION
Frames: 2
Frame Time: 0.0166667
1 100 2 0 0 0 0 0 0
1 100 3 0 90 0 0 0 90
";

    #[test]
    fn parses_hierarchy_channels_and_frames() {
        let clip = parse(SMALL, 0.01).unwrap();
        assert_eq!(clip.skeleton.names, ["Hips", "Spine", "Spine_End"]);
        assert_eq!(clip.skeleton.parents, [None, Some(0), Some(1)]);
        assert_eq!(clip.frame_count, 2);
        assert_eq!(clip.channels_per_frame, 9);
        assert!((clip.fps() - 60.0).abs() < 0.01);
        let mut out = vec![Transform::IDENTITY; 3];
        clip.frame_local(1, &mut out);
        assert!((out[0].translation - Vec3::new(0.01, 1.0, 0.03)).length() < 1e-6);
        // Xrotation 90 on the root: +Y goes to +Z.
        assert!((out[0].rotation.rotate(Vec3::Y) - Vec3::Z).length() < 1e-5);
        // Yrotation 90 on the spine: +Z goes to +X.
        assert!((out[1].rotation.rotate(Vec3::Z) - Vec3::X).length() < 1e-5);
        assert!((out[2].translation - Vec3::new(0.0, 0.2, 0.0)).length() < 1e-6);
    }

    #[test]
    fn rejects_short_motion() {
        let short = SMALL.replace("Frames: 2", "Frames: 3");
        assert!(parse(&short, 1.0).is_err());
    }
}
