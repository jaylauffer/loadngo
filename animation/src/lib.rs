//! Motion matching over captured motion, for loadngo's animation pipeline.
//!
//! The pieces, in the order the pipeline uses them:
//!
//! - [`math`], [`skeleton`]: vectors, quaternions, transforms, skeletons, forward kinematics.
//! - [`bvh`]: the BVH importer.
//! - [`retarget`]: from a capture's skeleton to a Mixamo-named target, by rest-pose
//!   alignment, with bone lengths and the hips' travel scaled to the target's legs.
//! - [`database`]: root trajectory, foot contacts, features, normalisation, and the
//!   compact binary file.
//! - [`search`]: the bounded nearest-feature search.
//! - [`spring`], [`path`], [`ik`]: springs and inertialization, steering, two-bone IK.
//! - [`controller`]: [`controller::MotionMatcher`], the per-frame runtime.
//! - [`eval`]: measures of foot slide, jerk, gait, hip sway and knees from a recording.
//! - `web` (wasm32 only): the browser's C-ABI surface.
//!
//! Pure Rust, no threads, no I/O: hosts bring the bytes (the pipeline tool reads and
//! writes through loadngo-proactor) and call [`controller::MotionMatcher::step`] only when
//! a frame asks. See `docs/ANIMATION.md`.

pub mod bvh;
pub mod controller;
pub mod database;
pub mod eval;
pub mod humanoid;
pub mod ik;
pub mod math;
pub mod path;
pub mod retarget;
pub mod search;
pub mod skeleton;
pub mod spring;
pub mod testing;
#[cfg(target_arch = "wasm32")]
mod web;
