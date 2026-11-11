//! An ASF/AMC importer (Acclaim skeleton and motion files, as the CMU Graphics Lab Motion
//! Capture Database publishes them).
//!
//! In ASF a bone runs from its parent's end along its rest `direction` (in the world frame at
//! rest) for its `length`, and turns about its own axes `C` (the `axis` angles). A frame's
//! rotation of a bone is `C · E · C⁻¹`, `E` the AMC's angles for its degrees of freedom
//! composed as `Rz · Ry · Rx` (fixed axes x, then y, then z), relative to its parent. Here
//! each bone becomes a joint at its start (its parent's end), with leaf ends `<bone>_End`,
//! the same shape the BVH importer gives. Lengths scale by `0.0254 / units.length` into metres
//! (CMU: `length 0.45`, so 0.056444).

use crate::math::{Quat, Transform, Vec3};
use crate::skeleton::Skeleton;

/// Why an ASF or AMC file could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsfError(pub String);

impl std::fmt::Display for AsfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ASF/AMC: {}", self.0)
    }
}

impl std::error::Error for AsfError {}

fn err<T>(m: impl Into<String>) -> Result<T, AsfError> {
    Err(AsfError(m.into()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dof {
    Rx,
    Ry,
    Rz,
    Tx,
    Ty,
    Tz,
}

/// A parsed ASF skeleton.
#[derive(Debug, Clone)]
pub struct Asf {
    /// Joints: `root`, every bone (at its start), and `<bone>_End` leaves.
    pub skeleton: Skeleton,
    /// Metres per file unit.
    pub scale: f32,
    /// Per joint: its `C` rotation and the degrees of freedom the AMC gives, in order.
    axes: Vec<Quat>,
    dofs: Vec<Vec<Dof>>,
}

fn euler_xyz_fixed(rx: f32, ry: f32, rz: f32) -> Quat {
    Quat::from_axis_angle(Vec3::Z, rz)
        * Quat::from_axis_angle(Vec3::Y, ry)
        * Quat::from_axis_angle(Vec3::X, rx)
}

fn axis_quat(order: &str, a: [f32; 3]) -> Quat {
    // The `axis` angles, applied in the order named, as fixed axes (x first for XYZ).
    let mut q = Quat::IDENTITY;
    for (k, c) in order.chars().enumerate() {
        let ax = match c.to_ascii_uppercase() {
            'X' => Vec3::X,
            'Y' => Vec3::Y,
            _ => Vec3::Z,
        };
        q = Quat::from_axis_angle(ax, a[k].to_radians()) * q;
    }
    q
}

fn dof_of(s: &str) -> Option<Dof> {
    Some(match s.to_ascii_lowercase().as_str() {
        "rx" => Dof::Rx,
        "ry" => Dof::Ry,
        "rz" => Dof::Rz,
        "tx" => Dof::Tx,
        "ty" => Dof::Ty,
        "tz" => Dof::Tz,
        _ => return None,
    })
}

struct Bone {
    name: String,
    dir: Vec3,
    len: f32,
    axis: Quat,
    dofs: Vec<Dof>,
}

/// Parses an ASF skeleton.
///
/// # Errors
/// [`AsfError`] for a malformed file or a hierarchy naming an unknown bone.
pub fn parse_asf(text: &str) -> Result<Asf, AsfError> {
    let mut unit_len = 1.0f32;
    let mut deg = true;
    let mut bones: Vec<Bone> = Vec::new();
    let mut root_order: Vec<Dof> = Vec::new();
    let mut root_axis = Quat::IDENTITY;
    let mut hierarchy: Vec<(String, Vec<String>)> = Vec::new();
    let mut section = String::new();
    let mut cur: Option<Bone> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix(':') {
            section = rest
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            continue;
        }
        let mut w = line.split_whitespace();
        let key = w.next().unwrap_or("").to_ascii_lowercase();
        let rest: Vec<&str> = w.collect();
        let num = |i: usize| -> Result<f32, AsfError> {
            rest.get(i)
                .and_then(|s| s.parse::<f32>().ok())
                .ok_or_else(|| AsfError(format!("{key}: a number expected in {line:?}")))
        };
        match section.as_str() {
            "units" => match key.as_str() {
                "length" => unit_len = num(0)?,
                "angle" => deg = !rest.first().is_some_and(|a| a.eq_ignore_ascii_case("rad")),
                _ => {}
            },
            "root" => match key.as_str() {
                "order" => root_order = rest.iter().filter_map(|s| dof_of(s)).collect(),
                "axis" => {}
                "orientation" => {
                    let a = [num(0)?, num(1)?, num(2)?];
                    root_axis = axis_quat("XYZ", if deg { a } else { a.map(f32::to_degrees) });
                }
                _ => {}
            },
            "bonedata" => match key.as_str() {
                "begin" => {
                    cur = Some(Bone {
                        name: String::new(),
                        dir: Vec3::Y,
                        len: 0.0,
                        axis: Quat::IDENTITY,
                        dofs: Vec::new(),
                    });
                }
                "end" => {
                    if let Some(b) = cur.take() {
                        if b.name.is_empty() {
                            return err("a bone without a name");
                        }
                        bones.push(b);
                    }
                }
                "name" => {
                    if let Some(b) = cur.as_mut() {
                        b.name = rest.first().map(|s| (*s).to_string()).unwrap_or_default();
                    }
                }
                "direction" => {
                    let d = Vec3::new(num(0)?, num(1)?, num(2)?);
                    if let Some(b) = cur.as_mut() {
                        b.dir = d.normalize_or_zero();
                    }
                }
                "length" => {
                    let l = num(0)?;
                    if let Some(b) = cur.as_mut() {
                        b.len = l;
                    }
                }
                "axis" => {
                    let a = [num(0)?, num(1)?, num(2)?];
                    let order = rest.get(3).copied().unwrap_or("XYZ");
                    if let Some(b) = cur.as_mut() {
                        b.axis = axis_quat(order, if deg { a } else { a.map(f32::to_degrees) });
                    }
                }
                "dof" => {
                    if let Some(b) = cur.as_mut() {
                        b.dofs = rest.iter().filter_map(|s| dof_of(s)).collect();
                    }
                }
                _ => {}
            },
            "hierarchy" => {
                if key == "begin" || key == "end" {
                    continue;
                }
                hierarchy.push((key.clone(), rest.iter().map(|s| (*s).to_string()).collect()));
            }
            _ => {}
        }
    }
    let scale = 0.0254 / unit_len.max(1e-6);
    let mut parent_of: Vec<Option<String>> = vec![None; bones.len()];
    for (p, children) in &hierarchy {
        for c in children {
            let i = bones
                .iter()
                .position(|b| b.name.eq_ignore_ascii_case(c))
                .ok_or_else(|| AsfError(format!("the hierarchy names an unknown bone {c:?}")))?;
            parent_of[i] = Some(p.clone());
        }
    }
    // Joints in parent-first order: root, then bones breadth-first.
    let mut names = vec!["root".to_string()];
    let mut parents: Vec<Option<usize>> = vec![None];
    let mut rest: Vec<Transform> = vec![Transform::IDENTITY];
    let mut axes = vec![root_axis];
    let mut dofs = vec![root_order];
    let mut joint_of_bone: Vec<Option<usize>> = vec![None; bones.len()];
    let mut queue = vec!["root".to_string()];
    let mut qi = 0;
    while qi < queue.len() {
        let pname = queue[qi].clone();
        qi += 1;
        let pj = names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(&pname))
            .unwrap_or(0);
        // The parent bone's end offset, where its children start.
        let offset = bones
            .iter()
            .find(|b| b.name.eq_ignore_ascii_case(&pname))
            .map_or(Vec3::ZERO, |b| b.dir * (b.len * scale));
        for (i, b) in bones.iter().enumerate() {
            if parent_of[i]
                .as_deref()
                .is_some_and(|p| p.eq_ignore_ascii_case(&pname))
                && joint_of_bone[i].is_none()
            {
                joint_of_bone[i] = Some(names.len());
                names.push(b.name.clone());
                parents.push(Some(pj));
                rest.push(Transform::new(offset, Quat::IDENTITY));
                axes.push(b.axis);
                dofs.push(b.dofs.clone());
                queue.push(b.name.clone());
            }
        }
    }
    // Leaf ends.
    for (i, b) in bones.iter().enumerate() {
        let Some(j) = joint_of_bone[i] else { continue };
        if !parents.contains(&Some(j)) {
            names.push(format!("{}_End", b.name));
            parents.push(Some(j));
            rest.push(Transform::new(b.dir * (b.len * scale), Quat::IDENTITY));
            axes.push(Quat::IDENTITY);
            dofs.push(Vec::new());
        }
    }
    let skeleton =
        Skeleton::new(names, parents, rest).map_err(|e| AsfError(format!("the skeleton: {e}")))?;
    Ok(Asf {
        skeleton,
        scale,
        axes,
        dofs,
    })
}

/// Parses an AMC motion for `asf`: the skeleton's local transforms, frame after frame.
///
/// # Errors
/// [`AsfError`] for a malformed file or a frame naming an unknown bone.
pub fn parse_amc(text: &str, asf: &Asf) -> Result<Vec<Transform>, AsfError> {
    let n = asf.skeleton.len();
    let mut out: Vec<Transform> = Vec::new();
    let mut frame: Option<Vec<Transform>> = None;
    let index: std::collections::HashMap<String, usize> = asf
        .skeleton
        .names
        .iter()
        .enumerate()
        .map(|(i, s)| (s.to_ascii_lowercase(), i))
        .collect();
    let mut deg = true;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with(':') {
            if line.eq_ignore_ascii_case(":radians") {
                deg = false;
            }
            continue;
        }
        let mut w = line.split_whitespace();
        let first = w.next().unwrap_or("");
        if first.chars().all(|c| c.is_ascii_digit()) {
            if let Some(f) = frame.take() {
                out.extend(f);
            }
            frame = Some(asf.skeleton.rest.clone());
            continue;
        }
        let Some(f) = frame.as_mut() else {
            return err("motion before the first frame number");
        };
        let Some(&j) = index.get(&first.to_ascii_lowercase()) else {
            return err(format!("an unknown bone {first:?}"));
        };
        let vals: Vec<f32> = w.filter_map(|s| s.parse::<f32>().ok()).collect();
        let (mut r, mut t) = ([0.0f32; 3], f[j].translation);
        for (k, d) in asf.dofs[j].iter().enumerate() {
            let Some(&v) = vals.get(k) else {
                return err(format!("{first}: too few values"));
            };
            let a = if deg { v.to_radians() } else { v };
            match d {
                Dof::Rx => r[0] = a,
                Dof::Ry => r[1] = a,
                Dof::Rz => r[2] = a,
                Dof::Tx => t.x = v * asf.scale,
                Dof::Ty => t.y = v * asf.scale,
                Dof::Tz => t.z = v * asf.scale,
            }
        }
        let c = asf.axes[j];
        f[j] = Transform::new(
            t,
            (c * euler_xyz_fixed(r[0], r[1], r[2]) * c.inverse()).normalize(),
        );
    }
    if let Some(f) = frame.take() {
        out.extend(f);
    }
    if out.is_empty() || !out.len().is_multiple_of(n) {
        return err("no frames");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skeleton::forward_kinematics;

    const ASF: &str = ":version 1.10
:name VICON
:units
  mass 1.0
  length 0.45
  angle deg
:root
   order TX TY TZ RX RY RZ
   axis XYZ
   position 0 0 0
   orientation 0 0 0
:bonedata
  begin
     id 1
     name lfemur
     direction 0 -1 0
     length 10
     axis 0 0 20  XYZ
    dof rx ry rz
  end
  begin
     id 2
     name ltibia
     direction 0 -1 0
     length 10
     axis 0 0 20  XYZ
    dof rx
  end
:hierarchy
  begin
    root lfemur
    lfemur ltibia
  end
";

    #[test]
    fn reads_bones_units_and_a_knee_bend() {
        let asf = parse_asf(ASF).unwrap();
        assert_eq!(
            asf.skeleton.names,
            ["root", "lfemur", "ltibia", "ltibia_End"]
        );
        assert!((asf.scale - 0.0254 / 0.45).abs() < 1e-6);
        let amc = ":FULLY-SPECIFIED\n:DEGREES\n1\nroot 0 17.7 0 0 0 0\nlfemur 0 0 0\nltibia 90\n";
        let f = parse_amc(amc, &asf).unwrap();
        let mut w = vec![Transform::IDENTITY; 4];
        forward_kinematics(&asf.skeleton.parents, &f, Transform::IDENTITY, &mut w);
        let s = asf.scale;
        assert!((w[0].translation.y - 17.7 * s).abs() < 1e-4);
        // The knee is 10 units below the hip; the shin turns 90 degrees about the tibia's own x
        // axis, which `axis 0 0 20` tilts 20 degrees about z: the ankle swings forward or back.
        assert!((w[2].translation - Vec3::new(0.0, (17.7 - 10.0) * s, 0.0)).length() < 1e-4);
        let shin = w[3].translation - w[2].translation;
        assert!((shin.length() - 10.0 * s).abs() < 1e-4);
        let d = shin.normalize_or_zero();
        assert!(
            (d - Vec3::new(-0.321, -0.117, -0.940)).length() < 0.01,
            "{d:?}"
        );
    }
}
