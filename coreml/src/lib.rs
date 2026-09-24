//! Apple adapter, deliberately separate from the unsafe-free portable API.
//! Graphs: a static dense projection (1x1 conv, baked weights) and a dynamic-weight
//! matmul for streamed weights ([`dense`]).
//! No Kimi architecture or Apache-derived code belongs in this crate.
#![deny(unsafe_code)]

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod apple;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod dense;
pub mod model;
#[cfg(target_os = "macos")]
pub use apple::{available_devices, CompiledModel, CoreMlModel};
