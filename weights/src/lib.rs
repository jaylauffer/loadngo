//! Model weight files and the numeric formats inside them.
//!
//! Every module is written from a published specification and knows nothing about any
//! particular model architecture:
//!
//! - [`safetensors`]: the safetensors container: an 8-byte little-endian header length, a
//!   UTF-8 JSON header, then a byte buffer every tensor indexes into.
//! - [`shards`]: a checkpoint split across several safetensors files, located through
//!   the `model.safetensors.index.json` weight map or by listing the directory.
//! - [`reader`]: tensor bytes read as positioned completion I/O through
//!   `loadngo-proactor`, a whole batch submitted before any read is awaited.
//! - [`dtype`]: element types, and exact widening of bfloat16 and IEEE 754 binary16.
//! - [`dense`]: matrix-vector products straight out of half-precision bytes.
//! - [`mxfp4`]: OCP Microscaling (MX) v1.0 MXFP4, 4-bit E2M1 elements sharing one E8M0
//!   scale per block of 32, multiplied without widening the matrix; ggml's block layout
//!   repacked into it.
//! - [`gguf`]: the GGUF container of the ggml project: metadata, tensor descriptions and
//!   aligned tensor data, read through [`reader::FileReader`].
//! - [`q8_0`]: ggml's `Q8_0` blocks (an `f16` scale and 32 signed bytes).

#![forbid(unsafe_code)]

pub mod dense;
pub mod dtype;
pub mod gguf;
pub mod mxfp4;
pub mod q8_0;
pub mod reader;
pub mod safetensors;
pub mod shards;
