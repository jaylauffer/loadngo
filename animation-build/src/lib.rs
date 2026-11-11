//! The animation pipeline's tools (see `docs/ANIMATION.md`):
//!
//! - [`io`]: file payloads through loadngo-proactor.
//! - [`lane`]: a bounded worker lane with thermal admission, results as proactor
//!   completions.
//! - [`gltf`]: target skeletons from glTF skins or JSON descriptions.
//! - [`clips`]: parse, trim, resample and retarget one capture; bone-map presets.
//! - [`scenarios`]: scripted walks for the evaluation harness.
//! - [`strip`]: side-on frame strips and top-down paths as PNG.

pub mod clips;
pub mod gltf;
pub mod io;
pub mod lane;
pub mod scenarios;
pub mod strip;
