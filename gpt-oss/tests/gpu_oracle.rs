//! The GPU forward pass against transformers' logits on the tiny random model
//! (`scripts/gpt_oss_tiny_oracle.py`): one position at a time with a sliding ring that
//! wraps, and passes large enough for the matrix units.
#![cfg(target_os = "macos")]

use std::path::Path;

use loadngo_gpt_oss::{
    gpu::{GpuModel, Logits},
    model::Model,
};
use serde_json::Value;

const TOLERANCE: f32 = 2e-3;

fn oracle() -> (Vec<u32>, usize, Vec<f32>) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny");
    let oracle: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("oracle.json")).unwrap()).unwrap();
    let ids = oracle["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as u32)
        .collect();
    let vocab = oracle["vocab"].as_u64().unwrap() as usize;
    let logits = std::fs::read(dir.join("logits.f32"))
        .unwrap()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    (ids, vocab, logits)
}

fn model() -> Model {
    Model::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny/model.gguf"))
        .unwrap()
}

/// Feeds the oracle's tokens in parts of the given sizes; returns the largest error.
fn largest_error(chunk: usize, parts: &[usize], gpu_routing: bool) -> f32 {
    largest_error_with(chunk, parts, gpu_routing, true)
}

fn largest_error_with(chunk: usize, parts: &[usize], gpu_routing: bool, pipeline: bool) -> f32 {
    let (ids, vocab, want) = oracle();
    let mut gpu = GpuModel::new(model(), chunk, 64).unwrap();
    gpu.gpu_routing = gpu_routing;
    gpu.pipeline = pipeline;
    let mut session = gpu.session().unwrap();
    let mut at = 0;
    let mut worst = 0.0_f32;
    for &part in parts {
        let logits = gpu
            .feed(&mut session, &ids[at..at + part], Logits::All)
            .unwrap();
        assert_eq!(logits.len(), part);
        for (i, row) in logits.iter().enumerate() {
            let expected = &want[(at + i) * vocab..][..vocab];
            let error = row
                .iter()
                .zip(expected)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f32::max);
            assert!(error < TOLERANCE, "position {}: max error {error}", at + i);
            worst = worst.max(error);
        }
        at += part;
    }
    assert_eq!(at, ids.len());
    worst
}

#[test]
fn one_position_at_a_time_through_a_wrapping_ring() {
    // Chunk 8: sliding layers keep (8 + 8) rounded up to 32 rows, so 40 positions wrap.
    // Routed on the GPU: each token one submission.
    let worst = largest_error(8, &[1; 40], true);
    eprintln!("one at a time, routed on the GPU: largest logit error {worst:e}");
    let worst = largest_error(8, &[1; 40], false);
    eprintln!("one at a time, routed on the CPU: largest logit error {worst:e}");
}

#[test]
fn passes_on_the_matrix_units_and_after_them() {
    let worst = largest_error(40, &[33, 7], true);
    eprintln!("33 then 7: largest logit error {worst:e}");
    let worst = largest_error(8, &[8, 8, 3, 8, 8, 5], true);
    eprintln!("chunks of 8: largest logit error {worst:e}");
}

#[test]
fn consecutive_passes_interleave_through_the_proactor() {
    // One call, several passes: pairs of passes interleaved layer by layer (and a last
    // odd one alone), against the same passes one after another.
    for (chunk, label) in [
        (8, "chunks of 8: two pairs and one alone"),
        (16, "chunks of 16: a pair and one alone"),
        (32, "chunks of 32: a tiled pair"),
    ] {
        let paired = largest_error_with(chunk, &[40], true, true);
        let alone = largest_error_with(chunk, &[40], true, false);
        eprintln!(
            "{label}: largest logit error {paired:e} interleaved, {alone:e} one after another"
        );
    }
}

#[test]
fn a_session_goes_back_and_reads_on_as_if_never_ahead() {
    let (ids, vocab, want) = oracle();
    let check = |logits: &[f32], pos: usize| {
        let error = logits
            .iter()
            .zip(&want[pos * vocab..][..vocab])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(error < TOLERANCE, "position {pos}: max error {error}");
    };
    // GPU: a sliding ring of (8 + 8) rounded up to 32 rows; 30 positions in, back 6.
    let gpu = GpuModel::new(model(), 8, 64).unwrap();
    let mut session = gpu.session_holding(64).unwrap();
    gpu.feed(&mut session, &ids[..30], Logits::Last).unwrap();
    assert!(
        session.truncate(12).is_err(),
        "further back than the ring allows"
    );
    session.truncate(24).unwrap();
    let logits = gpu.feed(&mut session, &ids[24..27], Logits::All).unwrap();
    for (i, row) in logits.iter().enumerate() {
        check(row, 24 + i);
    }
    // CPU reference: the same.
    let cpu = model();
    let mut session = cpu.session();
    for &id in &ids[..30] {
        cpu.step(&mut session, id);
    }
    session.truncate(24);
    for (i, &id) in ids[24..27].iter().enumerate() {
        check(&cpu.step(&mut session, id), 24 + i);
    }
}
