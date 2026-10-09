//! Rendering and fitting against the reference engine (`scripts/decider_fixtures.py
//! --prompts`): the state's and each question's token ids and the option positions, for
//! requests that fit and for ones that do not (a long state, a long question, 40
//! questions in two runs).

mod common;

use loadngo_decider::{prompt, tokenizer::Tokenizer};
use serde_json::Value;

fn ids(v: &Value) -> Vec<u32> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| u32::try_from(x.as_u64().unwrap()).unwrap())
        .collect()
}

#[test]
#[ignore = "needs the decider checkpoint (Hugging Face cache or DECIDER_CHECKPOINT)"]
fn every_request_renders_and_fits_as_the_reference_does() {
    let tokenizer = Tokenizer::from_file(&common::checkpoint().join("tokenizer.json")).unwrap();
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/prompt-parity.json")).unwrap();
    assert_eq!(cases.len(), 8);
    for case in &cases {
        let name = case["request"]["name"].as_str().unwrap();
        let (state, questions) = common::request(case);
        let rendered: Vec<_> = questions
            .iter()
            .map(|(_, q)| prompt::render_question(q))
            .collect();
        let fitted =
            prompt::fit(&tokenizer, &prompt::render_state(&state), &rendered, 4096).unwrap();
        let want = case["chunks"].as_array().unwrap();
        assert_eq!(fitted.len(), want.len(), "{name}: runs");
        for (got, want) in fitted.iter().zip(want) {
            assert_eq!(got.state, ids(&want["state_ids"]), "{name}: state");
            let q: Vec<Vec<u32>> = want["question_ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(ids)
                .collect();
            assert_eq!(got.question_ids, q, "{name}: questions");
            let o: Vec<Vec<usize>> = want["option_index"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| ids(row).into_iter().map(|i| i as usize).collect())
                .collect();
            assert_eq!(got.options, o, "{name}: option positions");
            let kinds: Vec<&str> = got
                .questions
                .clone()
                .map(|i| rendered[i].kind.name())
                .collect();
            let want_kinds: Vec<&str> = want["kinds"]
                .as_array()
                .unwrap()
                .iter()
                .map(|k| k.as_str().unwrap())
                .collect();
            assert_eq!(kinds, want_kinds, "{name}: kinds");
        }
    }
}
