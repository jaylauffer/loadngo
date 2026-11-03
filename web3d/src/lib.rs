//! loadngo-web3d: a lean WebGL2 3D renderer in Rust, compiled to WebAssembly, for skinned,
//! morph-targeted characters in small scenes on low-end GPUs such as the Raspberry Pi 5's V3D 7.1.
//! See `docs/WEB3D.md`.
//!
//! The crate has no dependencies. WebGL2 is bound by hand: `js/gl.js` owns the context and the
//! handle tables, and the wasm module imports its functions directly (module `web3d_gl`); the
//! exports in `web` (wasm32 only) are plain C-ABI functions that `js/web3d.js` wraps in a
//! `Renderer` class. Everything else is plain Rust, generic over [`gl::Gl`], and unit-tested on
//! the host against a recording mock, so no test needs a GL context.

pub mod gl;
pub mod math;
pub mod pack;
pub mod renderer;
pub mod shaders;
pub mod slots;
pub mod timer;

#[cfg(target_arch = "wasm32")]
pub mod web;

#[cfg(test)]
mod testing;

pub use renderer::{
    AlphaMode, Caps, Error, FrameStats, MaterialDesc, MeshInfo, MeshInput, Renderer, TextureDesc,
    Wrap,
};
