//! The GPU torso against the CPU reference (`model::Model`, `f32`) and against the
//! reference engine's answers, on the published checkpoint.
//!
//! The GPU holds the base weights in bfloat16, exactly as the files do, and the adapter
//! apart from them with `A` and `scale * B` rounded to bfloat16; activations stay `f32`.
//! So it differs from the CPU by that rounding and by the order of sums.
#![cfg(target_os = "macos")]

mod common;

use loadngo_decider::{
    gpu::GpuModel,
    model::{Adapter, Model},
    tokenizer::Tokenizer,
    Decider, Device,
};
use loadngo_inference::system_one::Request;
use serde_json::Value;

/// Largest probability difference allowed against the reference engine's fixture (CPU,
/// `f32`, adapter unrounded).
const TOLERANCE: f32 = 2e-3;

fn torso(merge: bool) -> Model {
    let checkpoint = common::checkpoint();
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(checkpoint.join("strands_decider_config.json")).unwrap(),
    )
    .unwrap();
    #[allow(clippy::cast_possible_truncation)]
    let scale =
        (config["lora_alpha"].as_f64().unwrap() / config["lora_r"].as_f64().unwrap()) as f32;
    Model::load(
        &common::base(),
        Some(Adapter {
            dir: &checkpoint.join("lora"),
            scale,
            merge,
        }),
        true,
    )
    .unwrap()
}

/// About 1,300 tokens of varied text: more than two GPU passes.
fn long_ids() -> Vec<u32> {
    let tokenizer = Tokenizer::from_file(&common::checkpoint().join("tokenizer.json")).unwrap();
    let mut text = String::new();
    for i in 0..40 {
        text.push_str(&format!(
            "Report {i}: the build on dolores finished in {} s; tests {} passed, {} failed. \
             Next: check the GPU path against the CPU, then hand off. ",
            17 + i * 3,
            200 + i,
            i % 3
        ));
    }
    tokenizer.encode(&text).into_iter().map(|t| t.id).collect()
}

#[test]
#[ignore = "needs the decider checkpoint and Qwen3.5-2B-Base; loads the model twice (~20 GB)"]
fn hidden_states_match_the_cpu_across_passes_and_session_copies() {
    let ids = long_ids();
    assert!(ids.len() > 1024, "{} tokens", ids.len());
    let cpu = torso(true);
    let h = cpu.config.hidden;
    let (head, tail) = ids.split_at(ids.len() - 40);
    let mut session = cpu.session();
    let mut want = cpu.forward(&mut session, head);
    want.extend(cpu.forward(&mut session.clone(), tail));
    drop(cpu);

    let gpu = GpuModel::new(torso(false), 512, 4096).unwrap();
    let mut session = gpu.session().unwrap();
    let all: Vec<usize> = (0..head.len()).collect();
    let mut got = gpu.read(&mut session, head, &all).unwrap();
    // The tail from a copy, as each question is read.
    let mut copy = gpu.session().unwrap();
    gpu.copy_session(&session, &mut copy).unwrap();
    let all: Vec<usize> = (0..tail.len()).collect();
    got.extend(gpu.read(&mut copy, tail, &all).unwrap());
    assert_eq!(got.len(), want.len());

    let mut worst = 0.0_f32;
    let mut worst_relative = 0.0_f32;
    for (p, (g, w)) in got.chunks(h).zip(want.chunks(h)).enumerate() {
        let norm = w.iter().map(|v| v * v).sum::<f32>().sqrt();
        let diff = g
            .iter()
            .zip(w)
            .map(|(a, b)| (a - b) * (a - b))
            .sum::<f32>()
            .sqrt();
        let largest = g
            .iter()
            .zip(w)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(largest.is_finite(), "position {p}");
        worst = worst.max(largest);
        worst_relative = worst_relative.max(diff / norm);
    }
    eprintln!(
        "{} positions: largest difference {worst:e}, largest relative (L2) {worst_relative:e}",
        ids.len()
    );
    assert!(
        worst_relative < 2e-3,
        "relative difference {worst_relative}"
    );
}

#[test]
#[ignore = "needs the decider checkpoint and Qwen3.5-2B-Base"]
fn answers_match_the_reference_engine() {
    let started = std::time::Instant::now();
    let decider = Decider::load_on(&common::checkpoint(), &common::base(), Device::Gpu).unwrap();
    eprintln!(
        "loaded on {} in {:.1}s",
        decider.device(),
        started.elapsed().as_secs_f64()
    );
    let fixture: Value = serde_json::from_str(include_str!("fixtures/e2e.json")).unwrap();
    let mut worst = 0.0_f32;
    for case in fixture["cases"].as_array().unwrap() {
        let (state, questions) = common::request(case);
        let started = std::time::Instant::now();
        let answers = decider
            .answer(&Request {
                state,
                questions: questions.clone(),
            })
            .unwrap();
        let elapsed = started.elapsed();
        for ((id, question), (got_id, got)) in questions.iter().zip(&answers) {
            assert_eq!(id, got_id);
            for (label, want) in common::expected(case, id, question) {
                let p = got
                    .probabilities
                    .iter()
                    .find(|(l, _)| *l == label)
                    .unwrap()
                    .1;
                let error = (p - want).abs();
                worst = worst.max(error);
                assert!(
                    error < TOLERANCE,
                    "{} {id} {label}: {p} vs {want}",
                    case["request"]["name"]
                );
            }
        }
        eprintln!(
            "{}: {:.3}s, largest error so far {worst:e}",
            case["request"]["name"],
            elapsed.as_secs_f64()
        );
    }
    eprintln!("largest probability error: {worst:e}");
    if let Some(time) = decider.take_gpu_time() {
        eprintln!("{time:?}");
    }
}

#[test]
#[ignore = "timing, by hand"]
fn time_a_long_read() {
    let ids = long_ids();
    let gpu = GpuModel::new(torso(false), 512, 4096).unwrap();
    let mut session = gpu.session().unwrap();
    gpu.read(&mut session, &ids[..512], &[]).unwrap();
    gpu.take_time();
    for _ in 0..3 {
        gpu.reset(&mut session).unwrap();
        gpu.read(&mut session, &ids[..1024], &[]).unwrap();
        eprintln!("{:?}", gpu.take_time());
    }
}
