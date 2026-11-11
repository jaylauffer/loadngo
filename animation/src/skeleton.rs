//! Skeletons and poses.
//!
//! A [`Skeleton`] is a list of named bones in topological order (every parent before its
//! children) with each bone's rest transform relative to its parent. A pose is one local
//! transform per bone; [`forward_kinematics`] turns it into world transforms.

use crate::math::{Quat, Transform, Vec3};

/// Bones, their parents and their rest (bind) local transforms.
#[derive(Debug, Clone, PartialEq)]
pub struct Skeleton {
    pub names: Vec<String>,
    /// `None` for a root.
    pub parents: Vec<Option<usize>>,
    /// Each bone's transform relative to its parent at rest.
    pub rest: Vec<Transform>,
}

/// Why a skeleton is not usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkeletonError {
    Empty,
    LengthMismatch,
    ParentAfterChild { bone: String },
    DuplicateName(String),
    NotFinite { bone: String },
}

impl std::fmt::Display for SkeletonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("the skeleton has no bones"),
            Self::LengthMismatch => {
                f.write_str("names, parents and rest transforms differ in length")
            }
            Self::ParentAfterChild { bone } => write!(f, "bone {bone:?} comes before its parent"),
            Self::DuplicateName(n) => write!(f, "two bones are named {n:?}"),
            Self::NotFinite { bone } => {
                write!(f, "bone {bone:?} has a rest transform that is not finite")
            }
        }
    }
}

impl std::error::Error for SkeletonError {}

impl Skeleton {
    /// Checks the invariants the rest of the crate relies on.
    ///
    /// # Errors
    /// [`SkeletonError`] when empty, inconsistent, out of order or not finite.
    pub fn new(
        names: Vec<String>,
        parents: Vec<Option<usize>>,
        rest: Vec<Transform>,
    ) -> Result<Self, SkeletonError> {
        if names.is_empty() {
            return Err(SkeletonError::Empty);
        }
        if names.len() != parents.len() || names.len() != rest.len() {
            return Err(SkeletonError::LengthMismatch);
        }
        for (i, p) in parents.iter().enumerate() {
            if let Some(p) = p {
                if *p >= i {
                    return Err(SkeletonError::ParentAfterChild {
                        bone: names[i].clone(),
                    });
                }
            }
            if !rest[i].translation.is_finite() || !rest[i].rotation.is_finite() {
                return Err(SkeletonError::NotFinite {
                    bone: names[i].clone(),
                });
            }
        }
        for (i, n) in names.iter().enumerate() {
            if names[..i].contains(n) {
                return Err(SkeletonError::DuplicateName(n.clone()));
            }
        }
        Ok(Self {
            names,
            parents,
            rest,
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.names.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// The index of the bone named `name`.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }

    /// World transforms of the rest pose.
    #[must_use]
    pub fn rest_world(&self) -> Vec<Transform> {
        let mut world = vec![Transform::IDENTITY; self.len()];
        forward_kinematics(&self.parents, &self.rest, Transform::IDENTITY, &mut world);
        world
    }

    /// The children of bone `i`.
    pub fn children(&self, i: usize) -> impl Iterator<Item = usize> + '_ {
        self.parents
            .iter()
            .enumerate()
            .filter_map(move |(c, p)| (*p == Some(i)).then_some(c))
    }
}

/// World transforms from local ones. `base` places the skeleton's roots.
///
/// `parents` must be in topological order (checked by [`Skeleton::new`]); `world` must be
/// as long as `local`. Allocates nothing.
pub fn forward_kinematics(
    parents: &[Option<usize>],
    local: &[Transform],
    base: Transform,
    world: &mut [Transform],
) {
    for i in 0..local.len() {
        world[i] = match parents[i] {
            Some(p) => world[p].compose(local[i]),
            None => base.compose(local[i]),
        };
    }
}

/// World positions only, from local rotations and translations. Allocates nothing.
pub fn forward_positions(
    parents: &[Option<usize>],
    local_rot: &[Quat],
    local_pos: &[Vec3],
    base: Transform,
    world_rot: &mut [Quat],
    world_pos: &mut [Vec3],
) {
    for i in 0..local_rot.len() {
        let (pr, pp) = match parents[i] {
            Some(p) => (world_rot[p], world_pos[p]),
            None => (base.rotation, base.translation),
        };
        world_pos[i] = pp + pr.rotate(local_pos[i]);
        world_rot[i] = pr * local_rot[i];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain() -> Skeleton {
        Skeleton::new(
            vec!["a".into(), "b".into(), "c".into()],
            vec![None, Some(0), Some(1)],
            vec![
                Transform::new(Vec3::new(0.0, 1.0, 0.0), Quat::IDENTITY),
                Transform::new(
                    Vec3::new(0.0, 0.5, 0.0),
                    Quat::from_axis_angle(Vec3::Z, 0.5),
                ),
                Transform::new(Vec3::new(0.0, 0.5, 0.0), Quat::IDENTITY),
            ],
        )
        .unwrap()
    }

    #[test]
    fn rest_world_composes_down_the_chain() {
        let s = chain();
        let w = s.rest_world();
        assert!((w[1].translation - Vec3::new(0.0, 1.5, 0.0)).length() < 1e-6);
        let expect = Vec3::new(0.0, 1.5, 0.0)
            + Quat::from_axis_angle(Vec3::Z, 0.5).rotate(Vec3::new(0.0, 0.5, 0.0));
        assert!((w[2].translation - expect).length() < 1e-6);
    }

    #[test]
    fn rejects_out_of_order_parents() {
        let e = Skeleton::new(
            vec!["a".into(), "b".into()],
            vec![Some(1), None],
            vec![Transform::IDENTITY; 2],
        );
        assert!(matches!(e, Err(SkeletonError::ParentAfterChild { .. })));
    }
}
