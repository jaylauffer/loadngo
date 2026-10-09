//! Where the published checkpoint and its base model are on this machine.

#![allow(dead_code)]

use std::path::PathBuf;

/// `StrandsAgents/strands-decider-2B-hobson-v21` at the revision the fixtures were made
/// with, from `DECIDER_CHECKPOINT` or the Hugging Face cache.
pub fn checkpoint() -> PathBuf {
    std::env::var_os("DECIDER_CHECKPOINT").map_or_else(
        || {
            PathBuf::from(std::env::var_os("HOME").unwrap()).join(
                ".cache/huggingface/hub/models--StrandsAgents--strands-decider-2B-hobson-v21/snapshots/2b52a6235c1b8306bbfa30b00b9d4b74b63a39f5",
            )
        },
        PathBuf::from,
    )
}

/// `Qwen/Qwen3.5-2B-Base` at the revision the decider was trained on, from `DECIDER_BASE`
/// or the Hugging Face cache.
pub fn base() -> PathBuf {
    std::env::var_os("DECIDER_BASE").map_or_else(
        || {
            PathBuf::from(std::env::var_os("HOME").unwrap()).join(
                ".cache/huggingface/hub/models--Qwen--Qwen3.5-2B-Base/snapshots/b1485b2fa6dfa1287294f269f5fb618e03d52d7c",
            )
        },
        PathBuf::from,
    )
}

use loadngo_inference::system_one::Question;
use serde_json::Value;

/// A fixture's request as `(state, [(id, question)])` in the fixture's question order.
/// A missing description is empty, as the reference renders it.
pub fn request(case: &Value) -> (String, Vec<(String, Question)>) {
    let r = &case["request"];
    let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    let questions = case["order"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| {
            let id = id.as_str().unwrap();
            let q = &r["questions"][id];
            let instructions = q["instructions"].as_str().map(str::to_owned);
            let question = match q["type"].as_str().unwrap() {
                "noul" => Question::Noul {
                    instructions: instructions.unwrap(),
                },
                "choice" => Question::Choice {
                    instructions,
                    criteria: q["criteria_order"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|l| {
                            let l = l.as_str().unwrap();
                            (l.to_owned(), text(&q["criteria"][l]))
                        })
                        .collect(),
                },
                "score" => Question::Score {
                    instructions,
                    criteria: q["criteria"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .enumerate()
                        .map(|(i, d)| (i.to_string(), text(d)))
                        .collect(),
                },
                other => panic!("unknown type {other}"),
            };
            (id.to_owned(), question)
        })
        .collect();
    (text(&r["state"]), questions)
}
