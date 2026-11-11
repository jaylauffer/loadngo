//! Target skeletons: from a glTF (`.glb` or `.gltf`) skin, or a JSON skeleton description.
//!
//! **glTF.** The joints of the skin that holds a `Hips` bone, at their nodes' own
//! transforms (the pose the file shows without animation). A joint whose parent is not a
//! joint becomes a root, its rest transform the joint's world transform; any scale on the
//! way is baked into the rest translations (so they are in the file's world units, metres
//! for glTF). Only the JSON is read; no buffers.
//!
//! **Skeleton description** (`.json`):
//!
//! ```json
//! { "bones": [ { "name": "Hips", "parent": null, "translation": [0, 1, 0], "rotation": [0, 0, 0, 1] },
//!              { "name": "Spine", "parent": "Hips", "translation": [0, 0.1, 0] } ] }
//! ```
//!
//! Parents come before children; `rotation` is optional (`[x, y, z, w]`).

use loadngo_animation::math::{Quat, Transform, Vec3};
use loadngo_animation::skeleton::Skeleton;
use serde_json::Value;

/// Why a skeleton could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkeletonFileError(pub String);

impl std::fmt::Display for SkeletonFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SkeletonFileError {}

fn e<T>(m: impl Into<String>) -> Result<T, SkeletonFileError> {
    Err(SkeletonFileError(m.into()))
}

/// A column-major 4×4 matrix.
type Mat = [f32; 16];

fn mat_mul(a: &Mat, b: &Mat) -> Mat {
    let mut r = [0.0; 16];
    for c in 0..4 {
        for row in 0..4 {
            r[c * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[c * 4 + k]).sum();
        }
    }
    r
}

fn trs(t: Vec3, q: Quat, s: Vec3) -> Mat {
    let x = q.rotate(Vec3::X) * s.x;
    let y = q.rotate(Vec3::Y) * s.y;
    let z = q.rotate(Vec3::Z) * s.z;
    [
        x.x, x.y, x.z, 0.0, y.x, y.y, y.z, 0.0, z.x, z.y, z.z, 0.0, t.x, t.y, t.z, 1.0,
    ]
}

/// The rigid part of a matrix (scale and shear dropped).
fn rigid(m: &Mat) -> Transform {
    let x = Vec3::new(m[0], m[1], m[2]).normalize_or_zero();
    let y0 = Vec3::new(m[4], m[5], m[6]);
    let y = (y0 - x * y0.dot(x)).normalize_or_zero();
    let z = x.cross(y);
    Transform::new(Vec3::new(m[12], m[13], m[14]), Quat::from_basis(x, y, z))
}

fn floats(v: Option<&Value>, n: usize) -> Option<Vec<f32>> {
    let a = v?.as_array()?;
    if a.len() != n {
        return None;
    }
    a.iter().map(|x| x.as_f64().map(|f| f as f32)).collect()
}

fn node_matrix(node: &Value) -> Mat {
    if let Some(m) = floats(node.get("matrix"), 16) {
        let mut r = [0.0; 16];
        r.copy_from_slice(&m);
        return r;
    }
    let t = floats(node.get("translation"), 3).map_or(Vec3::ZERO, |v| Vec3::new(v[0], v[1], v[2]));
    let q = floats(node.get("rotation"), 4).map_or(Quat::IDENTITY, |v| {
        Quat::new(v[0], v[1], v[2], v[3]).normalize()
    });
    let s = floats(node.get("scale"), 3)
        .map_or(Vec3::new(1.0, 1.0, 1.0), |v| Vec3::new(v[0], v[1], v[2]));
    trs(t, q, s)
}

/// The JSON of a `.glb` (its first chunk) or a `.gltf`.
///
/// # Errors
/// A malformed container or JSON.
pub fn gltf_json(bytes: &[u8]) -> Result<Value, SkeletonFileError> {
    let json: &[u8] = if bytes.len() >= 20 && &bytes[..4] == b"glTF" {
        let len = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
        if &bytes[16..20] != b"JSON" || 20 + len > bytes.len() {
            return e("the .glb's first chunk is not JSON");
        }
        &bytes[20..20 + len]
    } else {
        bytes
    };
    serde_json::from_slice(json).map_err(|x| SkeletonFileError(format!("glTF JSON: {x}")))
}

/// The skeleton of the skin holding a `Hips` joint (or the first skin).
///
/// # Errors
/// No skins, bad indices, or a skeleton that fails its checks.
pub fn skeleton_from_gltf(bytes: &[u8]) -> Result<Skeleton, SkeletonFileError> {
    let doc = gltf_json(bytes)?;
    let nodes = doc
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| SkeletonFileError("no nodes".into()))?;
    let skins = doc
        .get("skins")
        .and_then(Value::as_array)
        .ok_or_else(|| SkeletonFileError("no skins".into()))?;
    let name_of = |i: usize| {
        nodes
            .get(i)
            .and_then(|n| n.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
    };
    let joints_of = |s: &Value| -> Vec<usize> {
        s.get("joints")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|j| j.as_u64().map(|v| v as usize))
                    .collect()
            })
            .unwrap_or_default()
    };
    let skin = skins
        .iter()
        .find(|s| {
            joints_of(s)
                .iter()
                .any(|&j| loadngo_animation::humanoid::base_name(name_of(j)) == "Hips")
        })
        .or(skins.first())
        .ok_or_else(|| SkeletonFileError("no skins".into()))?;
    let joints = joints_of(skin);
    if joints.iter().any(|&j| j >= nodes.len()) {
        return e("a joint index is out of range");
    }
    // Parents of every node.
    let mut parent = vec![None; nodes.len()];
    for (i, n) in nodes.iter().enumerate() {
        if let Some(ch) = n.get("children").and_then(Value::as_array) {
            for c in ch.iter().filter_map(Value::as_u64) {
                if let Some(p) = parent.get_mut(c as usize) {
                    *p = Some(i);
                }
            }
        }
    }
    // World matrices.
    let mut world: Vec<Option<Mat>> = vec![None; nodes.len()];
    fn world_of(
        i: usize,
        nodes: &[Value],
        parent: &[Option<usize>],
        world: &mut Vec<Option<Mat>>,
        depth: usize,
    ) -> Mat {
        if let Some(m) = world[i] {
            return m;
        }
        let local = node_matrix(&nodes[i]);
        let m = match parent[i] {
            Some(p) if depth < 512 => {
                mat_mul(&world_of(p, nodes, parent, world, depth + 1), &local)
            }
            _ => local,
        };
        world[i] = Some(m);
        m
    }
    // Order the joints parents-first, by depth.
    let depth = |mut i: usize| {
        let mut d = 0;
        while let Some(p) = parent[i] {
            d += 1;
            i = p;
            if d > 512 {
                break;
            }
        }
        d
    };
    let mut ordered = joints.clone();
    ordered.sort_by_key(|&j| (depth(j), j));
    let mut names = Vec::new();
    let mut parents = Vec::new();
    let mut rest = Vec::new();
    for &j in &ordered {
        let wj = rigid(&world_of(j, nodes, &parent, &mut world, 0));
        // Nearest ancestor that is a joint.
        let mut p = parent[j];
        while let Some(pp) = p {
            if ordered.contains(&pp) {
                break;
            }
            p = parent[pp];
        }
        let (pi, local) = match p {
            Some(pp) => {
                let wp = rigid(&world_of(pp, nodes, &parent, &mut world, 0));
                (
                    ordered.iter().position(|&x| x == pp),
                    wp.inverse().compose(wj),
                )
            }
            None => (None, wj),
        };
        let mut name = name_of(j).to_string();
        if name.is_empty() || names.contains(&name) {
            name = format!("joint{j}");
        }
        names.push(name);
        parents.push(pi);
        rest.push(local);
    }
    Skeleton::new(names, parents, rest)
        .map_err(|x| SkeletonFileError(format!("the glTF skeleton: {x}")))
}

/// A skeleton description in JSON (see the module documentation).
///
/// # Errors
/// Malformed JSON, an unknown parent or a failing check.
pub fn skeleton_from_json(bytes: &[u8]) -> Result<Skeleton, SkeletonFileError> {
    let doc: Value = serde_json::from_slice(bytes)
        .map_err(|x| SkeletonFileError(format!("skeleton JSON: {x}")))?;
    let bones = doc
        .get("bones")
        .and_then(Value::as_array)
        .ok_or_else(|| SkeletonFileError("no \"bones\" array".into()))?;
    let mut names: Vec<String> = Vec::new();
    let mut parents = Vec::new();
    let mut rest = Vec::new();
    for b in bones {
        let name = b
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| SkeletonFileError("a bone has no name".into()))?;
        let parent = match b.get("parent") {
            None | Some(Value::Null) => None,
            Some(Value::String(p)) => Some(names.iter().position(|n| n == p).ok_or_else(|| {
                SkeletonFileError(format!(
                    "bone {name:?}: parent {p:?} is not listed before it"
                ))
            })?),
            Some(_) => return e(format!("bone {name:?}: parent must be a name or null")),
        };
        let t = floats(b.get("translation"), 3).ok_or_else(|| {
            SkeletonFileError(format!("bone {name:?}: translation must be 3 numbers"))
        })?;
        let q = match b.get("rotation") {
            None => Quat::IDENTITY,
            Some(_) => {
                let v = floats(b.get("rotation"), 4).ok_or_else(|| {
                    SkeletonFileError(format!("bone {name:?}: rotation must be 4 numbers"))
                })?;
                Quat::new(v[0], v[1], v[2], v[3]).normalize()
            }
        };
        names.push(name.to_string());
        parents.push(parent);
        rest.push(Transform::new(Vec3::new(t[0], t[1], t[2]), q));
    }
    Skeleton::new(names, parents, rest).map_err(|x| SkeletonFileError(format!("the skeleton: {x}")))
}

/// A skeleton as a description in JSON, the inverse of [`skeleton_from_json`].
#[must_use]
pub fn skeleton_to_json(s: &Skeleton) -> String {
    let bones: Vec<Value> = (0..s.len())
        .map(|i| {
            let t = s.rest[i].translation;
            let q = s.rest[i].rotation;
            serde_json::json!({
                "name": s.names[i],
                "parent": s.parents[i].map(|p| s.names[p].clone()),
                "translation": [t.x, t.y, t.z],
                "rotation": [q.x, q.y, q.z, q.w],
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({ "bones": bones })).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use loadngo_animation::testing::humanoid_skeleton;

    #[test]
    fn json_descriptions_round_trip() {
        let s = humanoid_skeleton(1.0);
        let back = skeleton_from_json(skeleton_to_json(&s).as_bytes()).unwrap();
        assert_eq!(back.names, s.names);
        assert_eq!(back.parents, s.parents);
        for (a, b) in s.rest.iter().zip(&back.rest) {
            assert!((a.translation - b.translation).length() < 1e-6);
        }
    }

    #[test]
    fn reads_a_minimal_gltf_skin_with_an_armature() {
        let gltf = r#"{
          "nodes": [
            { "name": "Armature", "children": [1], "scale": [0.01, 0.01, 0.01] },
            { "name": "Hips", "translation": [0, 100, 0], "children": [2] },
            { "name": "Spine", "translation": [0, 10, 0] },
            { "name": "Body", "skin": 0 }
          ],
          "skins": [ { "joints": [2, 1] } ]
        }"#;
        let s = skeleton_from_gltf(gltf.as_bytes()).unwrap();
        assert_eq!(s.names, ["Hips", "Spine"]);
        assert_eq!(s.parents, [None, Some(0)]);
        assert!((s.rest[0].translation - Vec3::new(0.0, 1.0, 0.0)).length() < 1e-5);
        assert!((s.rest[1].translation - Vec3::new(0.0, 0.1, 0.0)).length() < 1e-5);
    }
}
