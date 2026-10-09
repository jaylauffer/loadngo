//! The torso and head against transformers and peft on a random Qwen3.5 text model at toy
//! size (`scripts/decider_fixtures.py --tiny`): eight layers (three Gated DeltaNet, one
//! gated attention, twice), two key and four value heads in the DeltaNet layers, grouped
//! query attention, half the head dimensions rotated, and a random LoRA adapter on every
//! projection, over 70 positions.

use std::path::Path;

use loadngo_decider::{
    head::Head,
    model::{Adapter, Model},
};
use serde_json::Value;

const TOLERANCE: f32 = 1e-4;

fn fixture() -> (std::path::PathBuf, Value, Vec<f32>) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny");
    let oracle: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("oracle.json")).unwrap()).unwrap();
    let hidden = std::fs::read(dir.join("hidden.f32"))
        .unwrap()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    (dir, oracle, hidden)
}

fn load(dir: &Path) -> Model {
    Model::load(
        dir,
        Some(Adapter {
            dir: &dir.join("lora"),
            scale: 8.0 / 4.0,
        }),
        false,
    )
    .unwrap()
}

fn ids(oracle: &Value) -> Vec<u32> {
    oracle["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| u32::try_from(v.as_u64().unwrap()).unwrap())
        .collect()
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

#[test]
fn every_position_matches_transformers() {
    let (dir, oracle, want) = fixture();
    let model = load(&dir);
    let got = model.forward(&mut model.session(), &ids(&oracle));
    let error = worst(&got, &want);
    eprintln!("largest hidden-state error over 70 positions: {error:e}");
    assert!(error < TOLERANCE, "max error {error}");
}

#[test]
fn reading_in_pieces_gives_the_same_states_as_reading_whole() {
    let (dir, oracle, want) = fixture();
    let model = load(&dir);
    let ids = ids(&oracle);
    let mut session = model.session();
    let mut got = model.forward(&mut session, &ids[..33]);
    // A copy of the session continues exactly as the original would.
    let mut copy = session.clone();
    got.extend(model.forward(&mut copy, &ids[33..34]));
    got.extend(model.forward(&mut copy, &ids[34..]));
    assert_eq!(copy.len(), 70);
    assert!(worst(&got, &want) < TOLERANCE);
}

#[test]
fn the_head_scores_options_as_the_reference_does() {
    let (dir, oracle, want) = fixture();
    let head = Head::load(&dir, 64, 16).unwrap();
    let row = |p: usize| &want[p * 64..(p + 1) * 64];
    let options: Vec<&[f32]> = oracle["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| row(p.as_u64().unwrap() as usize))
        .collect();
    let logits = head.logits(row(69), &options);
    let expected: Vec<f32> = oracle["head_logits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap() as f32)
        .collect();
    assert!(
        worst(&logits, &expected) < 1e-5,
        "{logits:?} vs {expected:?}"
    );
}
