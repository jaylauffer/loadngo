//! OpenAI's gpt-oss models, read from their GGUF files.
//!
//! Written from the published architecture (OpenAI's model card and transformers'
//! `GptOss` implementation, both Apache 2.0) and the GGUF and OCP MX specifications;
//! the weight formats are `loadngo-weights`'.

#![forbid(unsafe_code)]

pub mod config;
pub mod model;
pub mod tokenizer;
