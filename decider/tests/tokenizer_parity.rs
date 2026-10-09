//! The tokenizer against Hugging Face `tokenizers` with the checkpoint's own
//! `tokenizer.json` (`scripts/decider_fixtures.py --tokenizer`).

mod common;

use loadngo_decider::tokenizer::Tokenizer;
use serde_json::Value;

#[test]
#[ignore = "needs the decider checkpoint (Hugging Face cache or DECIDER_CHECKPOINT)"]
fn every_text_encodes_token_for_token_with_its_offsets() {
    let tokenizer = Tokenizer::from_file(&common::checkpoint().join("tokenizer.json")).unwrap();
    assert_eq!(tokenizer.vocab_size(), 248_077);
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/tokenizer-parity.json")).unwrap();
    let mut tokens = 0;
    for case in &cases {
        let text = case["text"].as_str().unwrap();
        let got = tokenizer.encode(text);
        let ids: Vec<u64> = got.iter().map(|t| u64::from(t.id)).collect();
        let want: Vec<u64> = case["ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap())
            .collect();
        assert_eq!(ids, want, "{text:?}");
        // Offsets count normalized characters where NFC changed the text.
        if icu_free_nfc(text) {
            let offsets: Vec<(u64, u64)> =
                got.iter().map(|t| (t.start as u64, t.end as u64)).collect();
            let want: Vec<(u64, u64)> = case["offsets"]
                .as_array()
                .unwrap()
                .iter()
                .map(|o| (o[0].as_u64().unwrap(), o[1].as_u64().unwrap()))
                .collect();
            assert_eq!(offsets, want, "{text:?}");
        }
        tokens += got.len();
    }
    assert_eq!((cases.len(), tokens), (23, 439));
}

/// Whether `text` has no decomposed sequence NFC would compose (the fixtures' only such
/// text is the one that says so).
fn icu_free_nfc(text: &str) -> bool {
    !text.starts_with("Decomposed")
}
