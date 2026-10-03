//! The forward pass against transformers' `GptOssForCausalLM` on a random-weight model
//! with the 20B's structure at toy size (`scripts/gpt_oss_tiny_oracle.py`): 4 layers,
//! a sliding window of 8 over 40 positions, Q8_0 attention and MXFP4 experts.

use std::path::Path;

use loadngo_gpt_oss::model::Model;
use serde_json::Value;

const TOLERANCE: f32 = 2e-3;

#[test]
fn every_position_matches_transformers() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny");
    let oracle: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("oracle.json")).unwrap()).unwrap();
    let vocab = oracle["vocab"].as_u64().unwrap() as usize;
    let want: Vec<f32> = std::fs::read(dir.join("logits.f32"))
        .unwrap()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    let model = Model::load(&dir.join("model.gguf")).unwrap();
    assert_eq!(model.config.vocab, vocab);
    let mut session = model.session();
    let mut worst = 0.0_f32;
    for (pos, id) in oracle["ids"].as_array().unwrap().iter().enumerate() {
        let logits = model.step(&mut session, id.as_u64().unwrap() as u32);
        let expected = &want[pos * vocab..][..vocab];
        let error = logits
            .iter()
            .zip(expected)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(error < TOLERANCE, "position {pos}: max error {error}");
        worst = worst.max(error);
    }
    eprintln!("largest logit error over 40 positions: {worst:e}");
}
