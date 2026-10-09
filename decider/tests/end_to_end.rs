//! The published checkpoint against its reference engine on the CPU in `f32`
//! (`scripts/decider_fixtures.py --e2e`): five synthetic requests of every question kind.

mod common;

use loadngo_decider::Decider;
use loadngo_inference::system_one::Request;
use serde_json::Value;

/// The fixture rounds probabilities to four places (5e-5); the largest difference
/// measured is 4.6e-5 (2026-10-10).
const TOLERANCE: f32 = 1e-4;

#[test]
#[ignore = "needs the decider checkpoint and Qwen3.5-2B-Base (Hugging Face cache, DECIDER_CHECKPOINT, DECIDER_BASE); ~2 minutes on the CPU"]
fn every_answer_matches_the_reference_engine() {
    let started = std::time::Instant::now();
    let decider = Decider::load(&common::checkpoint(), &common::base()).unwrap();
    eprintln!("loaded in {:.1}s", started.elapsed().as_secs_f64());
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
        for ((id, question), (got_id, got)) in questions.iter().zip(&answers) {
            assert_eq!(id, got_id);
            let want = &case["answers"][id];
            let p = |label: &str| {
                got.probabilities
                    .iter()
                    .find(|(l, _)| l == label)
                    .unwrap()
                    .1
            };
            let expected: Vec<(String, f32)> = match want["type"].as_str().unwrap() {
                #[allow(clippy::cast_possible_truncation)]
                "noul" => vec![("true".into(), want["noul"].as_f64().unwrap() as f32)],
                _ => want["probabilities"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| {
                        // A score's levels are numbered; ours keep the request's labels in order.
                        let label = match question {
                            loadngo_inference::system_one::Question::Score { criteria, .. } => {
                                criteria[k.parse::<usize>().unwrap()].0.clone()
                            }
                            _ => k.clone(),
                        };
                        #[allow(clippy::cast_possible_truncation)]
                        (label, v.as_f64().unwrap() as f32)
                    })
                    .collect(),
            };
            for (label, want) in expected {
                let error = (p(&label) - want).abs();
                if std::env::var_os("DECIDER_E2E_VERBOSE").is_some() {
                    eprintln!(
                        "  {} {id} {label}: {:.5} vs {want:.4} ({error:.1e})",
                        case["request"]["name"],
                        p(&label)
                    );
                }
                worst = worst.max(error);
                assert!(
                    error < TOLERANCE,
                    "{} {id} {label}: {} vs {want}",
                    case["request"]["name"],
                    p(&label)
                );
            }
        }
        eprintln!(
            "{}: {:.1}s, largest error so far {worst:e}",
            case["request"]["name"],
            started.elapsed().as_secs_f64()
        );
    }
    eprintln!("largest probability error: {worst:e}");
}
