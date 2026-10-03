//! The tokenizer against Hugging Face `tokenizers` on the same vocabulary and merges
//! (`scripts/gpt_oss_tokenizer_fixture.py`).

use std::path::PathBuf;

use loadngo_gpt_oss::tokenizer::Tokenizer;
use loadngo_weights::gguf;
use serde_json::Value;

fn model() -> PathBuf {
    std::env::var_os("GPT_OSS_GGUF").map_or_else(
        || {
            PathBuf::from(std::env::var_os("HOME").unwrap()).join(
                ".loadngo/models/56fcc05caeabd1f4f352f7b9d6762cad2035b860973c07a5c330a7fa3944e8e1.gguf",
            )
        },
        PathBuf::from,
    )
}

#[test]
#[ignore = "needs gpt-oss-20b's GGUF (~/.loadngo/models or GPT_OSS_GGUF)"]
fn every_text_encodes_token_for_token_and_decodes_back() {
    let (file, _) = gguf::open(&model()).unwrap();
    let tokenizer = Tokenizer::from_gguf(&file).unwrap();
    assert_eq!(tokenizer.vocab_size(), 201_088);
    assert_eq!(tokenizer.control("<|start|>"), Some(200_006));
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/tokenizer-parity.json")).unwrap();
    let mut tokens = 0;
    for case in &cases {
        let text = case["text"].as_str().unwrap();
        let want: Vec<u32> = case["ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_u64().unwrap() as u32)
            .collect();
        let got = tokenizer.encode(text);
        assert_eq!(got, want, "{text:?}");
        assert_eq!(tokenizer.decode(&got), text);
        assert!(got.iter().all(|&id| !tokenizer.is_control(id)));
        tokens += got.len();
    }
    assert_eq!((cases.len(), tokens), (26, 5132));
}
