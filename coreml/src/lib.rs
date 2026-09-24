//! Apple adapter, deliberately separate from the unsafe-free portable API.
//! The first graph is a static dense projection, represented as a 1x1 conv.
//! No Kimi architecture or Apache-derived code belongs in this crate.
#![deny(unsafe_code)]

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod apple;
pub mod model;
#[cfg(target_os = "macos")]
pub use apple::{available_devices, CompiledModel, CoreMlModel};
