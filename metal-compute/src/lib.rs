//! GPU compute for local models on Apple Metal.
//!
//! Weights live in shared-storage Metal buffers, uploaded once. Work is recorded into a
//! [`Batch`] (one command buffer, one compute encoder) and committed whole; the GPU's
//! completion is delivered as a job on a loadngo proactor, together with the buffers the
//! batch owned, so nothing waits on the GPU and the CPU never reads a buffer the GPU may
//! still be writing. Plan and phases: `docs/METAL_COMPUTE_PLAN.md`.
//!
//! Kernels (MSL, compiled from source when a [`Gpu`] is created):
//! - `gemv_bf16`: `y = W x` with `W` bfloat16, row-major.
//! - `gemv_mxfp4`: `y = W x` with `W` in OCP MX v1.0 MXFP4 (the layout of
//!   `loadngo_weights::mxfp4::Mxfp4Matrix`: packed elements and one scale per 32).
//! - `gemv_q8_0`, `gemm_q8_0`, `gemm_q8_0_tiled`: products with ggml's `Q8_0` (a
//!   binary16 scale per 32 signed bytes), repacked for aligned loads.
//! - `attention_split_key`: causal multi-head attention whose keys are split between a
//!   per-head part and a part every head shares (multi-head latent attention's layout).
//! - `attention_grouped`: causal grouped-query attention, optionally over a sliding
//!   window, with keys and values in a ring of rows, and optionally a learned sink
//!   logit per head.
//! - Glue between products: norms, row copies and adds, rotary by halves from a cos/sin
//!   table, gpt-oss's clamped SwiGLU.
//! - `delta_rule_recurrence`: the delta-rule linear-attention recurrence with a decay per
//!   key channel (Kimi Delta Attention), steps in order, state updated in place.
//!
//! No model architecture belongs in this crate.
#![deny(unsafe_code)]

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod apple;

#[cfg(target_os = "macos")]
pub use apple::{
    AttentionShape, Batch, Buffer, Completed, Dispatch, Gpu, GroupedShape, RecurrenceShape,
    Resident, Rows, Slice,
};

/// Why a GPU operation could not be set up or did not complete.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no Metal device")]
    NoDevice,
    #[error("Metal kernel compilation failed: {0}")]
    Compile(String),
    #[error("could not allocate a {0}-byte Metal buffer")]
    Alloc(usize),
    #[error("invalid dispatch: {0}")]
    Dispatch(String),
    #[error("GPU command buffer failed: {0}")]
    Gpu(String),
}
