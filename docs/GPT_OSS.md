# gpt-oss in loadngo

OpenAI's gpt-oss-20b, run by Rust code in this repository, so the Zhoenus head model
and Kimi's eventual orchestrator need neither `llama-server` nor the Apache-licensed
kimi engine. Started 2026-10-04 at Jay's request. The code is written from:

- the GGUF specification (ggml `docs/gguf.md`, version 3);
- OCP MX v1.0;
- the published architecture: OpenAI's model card, and transformers' `GptOss` code and
  YaRN rotary (Apache 2.0), read as a specification.

Nothing is translated from kimi-k3-in-rust (Jay's 2026-09-16 rule for this BSD-3
repository).

## The model file

gpt-oss-20b is one GGUF, held by content: Archive CAS object
`56fcc05caeabd1f4f352f7b9d6762cad2035b860973c07a5c330a7fa3944e8e1` (12,109,565,760
bytes). It is in the signed pudding CAS, and its verified local copy is
`~/.loadngo/models/<hash>.gguf` (see `docs/ZHOENUS_HEAD_MODEL_RUNNER.md`).

- Weight types: 72 MXFP4 expert tensors, 98 `Q8_0` (embedding, output, attention) and
  289 `f32` (norms, biases, sinks, routers).
- Shape: 24 layers, hidden width 2,880, 64 query heads and 8 key/value heads of 64,
  32 experts with 4 used, and a vocabulary of 201,088.

## Crates

- **`loadngo-weights`** (`weights/`):
  - `gguf`: header, metadata and tensor descriptions, read through the proactor;
  - `q8_0`: rows and products;
  - `mxfp4::ggml_to_ocp`: ggml's MXFP4 blocks repacked exactly into `Mxfp4Matrix`'s
    layout, so the existing MXFP4 code applies, Metal kernels included;
  - `reader::FileReader`: byte ranges of one file through the proactor.
- **`loadngo-gpt-oss`** (`gpt-oss/`):
  - `tokenizer`: the o200k byte-level BPE, with a hand-written scanner for the
    `gpt-4o` split pattern. The pattern's lookahead is beyond Rust's `regex`; the
    Unicode classes come from `regex-syntax`.
  - `config`: the model's shape from `gpt-oss.*` metadata, and the YaRN frequencies.
  - `model`: the CPU reference forward pass with a key/value cache. Matrices stay in
    their file formats, and large products are split across the cores.
  - `gpt_oss_generate`: greedy continuation from the command line.

## Evidence (2026-10-04, M4 Pro Mac mini)

- **The file against ggml's own reader.** On the real GGUF, 9 rows and every offset are
  identical to ggml's Python `gguf` reader:
  - `f32` sinks;
  - `Q8_0` embedding and attention;
  - MXFP4 experts in layers 3 and 23.

  Test: `weights/tests/gguf_gpt_oss.rs`, ignored without the model. Fixture:
  `scripts/gguf_reference_rows.py`.
- **Tokenizer.** Token for token with Hugging Face `tokenizers` (0.23.2) on the same
  vocabulary, merges and pattern: 26 texts, 5,132 tokens. The texts cover English,
  code, Chinese, Japanese, Korean, Russian, Greek, Arabic, emoji with modifiers and
  joiners, combining marks, contractions including the long s, and whitespace and
  line-ending edge cases. Every text also decodes back exactly. Test:
  `gpt-oss/tests/tokenizer_parity.rs`; fixture: `scripts/gpt_oss_tokenizer_fixture.py`.
- **Forward pass.** Against transformers' `GptOssForCausalLM` (5.19.0.dev0, `float32`,
  eager attention) on a random model with the 20B's structure at toy size:
  - 4 layers and a sliding window of 8 over 40 positions;
  - `Q8_0` attention and MXFP4 experts, large enough that the clamps at 7 act;
  - transformers runs on exactly the quantized weights.

  The largest logit error at any position is 5.8e-5. Test:
  `gpt-oss/tests/tiny_oracle.rs`; fixture: `scripts/gpt_oss_tiny_oracle.py`. Eight
  deliberate bugs each fail it, by 0.025-5.0:
  - no sinks;
  - a window one narrower;
  - sliding on the odd layers;
  - no gate clamp;
  - no +1 on up;
  - a flipped rotation sign;
  - no YaRN scale;
  - no YaRN interpolation.
- **The real model.** "The capital of France is" continues with " Paris." (greedy, no
  chat format):
  - loaded in 6.6 s;
  - 0.5 s per token on the CPU;
  - peak memory 29 GB, because raw and repacked tensors coexist during load.

## Next

1. **The harmony chat format.** `<|start|>role<|message|>…<|end|>` and the channels,
   checked against the template kept in kimi-k3-in-rust
   (`tests/fixtures/gpt-oss/chat_template.jinja`).
2. **The GPU.** The weights resident in Metal arenas, and the existing loadngo kernels
   for the products:
   - `gemm_mxfp4_tiled` and the GEMVs for the experts;
   - a `Q8_0` kernel for attention and the output;
   - `attention_grouped` for the sliding window, with the sink added.

   The CPU path stays as the reference.
3. **Load without the double copy.** Repack each tensor as it arrives.
4. **The model service.** `network::model_service` runs this engine instead of
   `llama-server`, keeping its by-hash model resolution.
