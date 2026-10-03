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
  - `chat`: the harmony chat format without tools: system, developer, user and earlier
    assistant messages, with `read_reply` splitting a reply into its analysis and final
    channels. Text that spells a control token stays text.
  - `gpt_oss_generate`: greedy continuation, or one chat answer with `--chat`, from the
    command line.

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

- **Chat format.** Three conversations render token for token as gpt-oss's own chat
  template does, through Jinja2 and Hugging Face `tokenizers` (244 tokens). They cover:
  - instructions;
  - an earlier answer, whose reasoning is dropped;
  - low and high reasoning;
  - another identity;
  - Chinese, emoji and indentation.

  The template is kept in kimi-k3-in-rust (`tests/fixtures/gpt-oss/chat_template.jinja`).
  Test: `gpt-oss/tests/chat_parity.rs`; fixture: `scripts/gpt_oss_chat_fixture.py`.

  One deliberate difference: `tokenizers` turns `<|start|>` typed inside a message into
  the control token, so a user could forge a message boundary. Here it stays text
  (tested).

  Real model, `--chat 'What is the capital of France? Answer in one sentence.'` at low
  reasoning: analysis "Answer: Paris.", final answer "Paris.", ended by `<|return|>`.
  The 79-token prompt took 39.5 s, one position at a time on the CPU; then 16 tokens
  at 0.47 s each.

## The GPU path (`gpu`, macOS)

`GpuModel` copies the weights into GPU memory in their file formats and releases the
CPU copies; the CPU keeps the router, the embedding and the rotary frequencies. Each
layer is two submissions, each completing on a loadngo proactor:

1. **Attention.** Norm; q, k and v (`Q8_0`) with biases; rotary by halves from a
   cos/sin table computed on the CPU in f64; the new rows into the layer's cache;
   grouped attention with sinks; the output projection, its bias and the residual; the
   norm before the experts.
2. **Experts.** The CPU routes every position from the normalized rows, read straight
   from shared memory, and gathers them by expert. The GPU computes each chosen expert:
   MXFP4 gate and up, the clamped SwiGLU, down and its bias. The CPU adds the weighted
   results to the residual.

Prompt passes take up to 512 positions. From 32 positions every product and the
attention run on the matrix units. Sliding layers keep a ring of 640 rows (128 + 512);
full layers keep `max_context` rows.

The kernels added to `loadngo-metal-compute` for this:

- `gemv_q8_0`, `gemm_q8_0`, `gemm_q8_0_tiled`;
- sinks in `attention_grouped` and `attention_grouped_tiled`;
- `add_rows`, `rotate_halves`, `clamped_swiglu`.

They are tested against float64 (`metal-compute/tests/q8_0_and_gpt_oss_glue.rs`,
`attention_grouped.rs`), with mutations caught: no sink gives an error of 0.96, a
`Q8_0` without its scale 0.79.

**Evidence (2026-10-04, M4 Pro):**

- **Tiny-model oracle.** Against transformers (`gpt-oss/tests/gpu_oracle.rs`): largest
  logit error 2.3e-5 to 3.2e-5 for each of:
  - one position at a time, through a sliding ring that wraps;
  - passes of 8;
  - a 33-position pass on the matrix units.
- **Real gpt-oss-20b** (`gpt_oss_generate --gpu`):
  - "The capital of France is" -> " Paris." (as on the CPU), decoding at 40.1 tokens/s
    (CPU: 2);
  - a 4,246-token prompt (`docs/ARCHIVE_CAS.md`) in 15.1 s (282 tokens/s), then 23.4
    tokens/s at that context;
  - chat, "In two sentences: why does a Rust program need both a borrow checker and
    lifetimes?": a correct two-sentence answer, 85-token prompt in 0.9 s, 37 tokens/s;
  - loads in 6.5-10.7 s; peak resident 29.6 GB during load.

## The Neural Engine

Jay asked whether gpt-oss can use it. Not for decoding:

- Decoding reads about 2.5 GB of weights per token (4 experts, attention and the
  output), so it is limited by memory bandwidth.
- The GPU reads at about 180-230 GB/s. loadngo-coreml's weight streaming to the Neural
  Engine measured 24-29 GB/s (kimi-k3-in-rust `docs/KIMI_LINEAR.md`).

Prompt attention could move there, as for Gemma (loadngo `docs/NPU_ACCELERATION.md`):

- The sink is one more column in the Core ML softmax.
- But gpt-oss's attention is small: 64-wide heads, and half the layers see only 128
  positions. As for Kimi Linear, expect a few percent; measure attention's share of a
  prompt before building it.

The Neural Engine's better use is running other models beside gpt-oss, for example
speech recognition or embeddings. Their completions already arrive through the
proactor.

## Next

1. **Profile the GPU path.**
   - Why decoding drops from 40 to 23 tokens/s by 4k positions.
   - How much of a prompt is CPU routing, gathering and scattering.
   - Then route on the GPU, so a layer is one submission.
2. **Tools in the chat format.** Declarations in the developer message, and calls on
   the commentary channel to `functions.NAME`.
3. **Load without the double copy.** Repack each tensor as it arrives (peak now
   29.6 GB).
4. **The model service.** `network::model_service` runs this engine instead of
   `llama-server`, keeping its by-hash model resolution.
