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
CPU copies. A layer's experts sit one after another in one buffer per matrix and bias,
so the GPU can pick them by index. The CPU keeps the embedding, the rotary frequencies
and a copy of the router for prompts. Every submission completes on a loadngo proactor.

- **Decoding (one new position)** is one submission per token. For each layer:
  attention, then the router (`gemv_f32`), `topk_softmax`, the chosen experts
  (`gemv_mxfp4_selected` for gate, up and down), `clamped_swiglu_selected` and
  `moe_combine` into the residual. Then the output.
- **Prompts** take two submissions per layer:
  1. Attention: norm; q, k and v (`Q8_0`) with biases; rotary by halves from a cos/sin
     table computed on the CPU in f64; the new rows into the layer's cache; grouped
     attention with sinks; the output projection, its bias and the residual; the norm
     before the experts.
  2. Experts: the CPU routes every position from the normalized rows, read straight
     from shared memory, and gathers them by expert; the GPU computes each chosen
     expert; the CPU adds the weighted results to the residual.

- **Two prompt passes in a row** are interleaved layer by layer, each step triggered by
  a completion the proactor delivers:
  - when the first pass's attention for a layer finishes, the second's is queued at
    once, and the CPU routes the first while the GPU works;
  - when a pass's experts finish, the CPU adds them to its residual and that pass's
    next attention is queued.

  Attention stays in order, first pass then second, layer by layer: the second pass
  sees the first's keys. The passes share the caches; their other buffers are double.
  Each layer's experts run side by side in one concurrent batch, with barriers between
  gate/up, SwiGLU, down and biases.

Prompt passes take up to 512 positions. From 32 positions every product and the
attention run on the matrix units. Sliding layers keep a ring of 640 rows (128 + 512);
full layers keep `max_context` rows. `gpt_oss_generate --gpu --profile` prints where the
time went: per submission kind, the CPU encoding, the GPU's own execution time (Metal's
timestamps, carried in the proactor completion) and the wall time; and the CPU work
between submissions.

Kernels added to `loadngo-metal-compute` for this, each tested against float64:

- `gemv_q8_0`, `gemm_q8_0`, `gemm_q8_0_tiled`;
- sinks in grouped attention;
- `attention_grouped_narrow`: heads up to 128 wide, each lane scoring its own key;
- `add_rows`, `rotate_halves`, `clamped_swiglu`;
- for routing: `gemv_f32`, `topk_softmax`, `gemv_mxfp4_selected`,
  `clamped_swiglu_selected`, `moe_combine`.

Mutations caught by the tests:

- no sink (error 0.96);
- a `Q8_0` without its scale (0.79);
- the narrow kernel not rescaling (0.014).

The routing test caught a real bug first: top-k overwrote its maximum before
subtracting it (weight 0.16 for 0.46).

**Evidence (2026-10-04, M4 Pro):**

- **Tiny-model oracle.** Against transformers (`gpt-oss/tests/gpu_oracle.rs`), the
  largest logit error is 2.3e-5 to 3.2e-5 for each of:
  - one position at a time through a wrapping ring, routed on the GPU and on the CPU;
  - passes of 8;
  - a 33-position pass on the matrix units.
- **Real gpt-oss-20b** (`gpt_oss_generate --gpu`):
  - "The capital of France is" -> " Paris.";
  - a 4,246-token prompt (`docs/ARCHIVE_CAS.md`) in 15.1-15.4 s (276-282 tokens/s);
  - chat, "In two sentences: why does a Rust program need both a borrow checker and
    lifetimes?": a correct two-sentence answer;
  - loads in 6.5-10.7 s; peak resident 29.6 GB during load.

Decoding speed, as each step landed:

| | Short context | After 4,246 tokens |
|---|---:|---:|
| Two submissions per layer, routed on the CPU | 25.9 ms (38-40 tokens/s) | 48.6 ms (20-23 tokens/s) |
| Routed on the GPU, one submission per token | 16.6 ms (59 tokens/s) | 33.0 ms (30 tokens/s) |
| + `attention_grouped_narrow` | 17.0 ms (57-59 tokens/s) | 22.8 ms (43 tokens/s) |

At short context, decoding is near the bandwidth floor: about 2.5 GB per token at about
200 GB/s is about 12.5 ms.

Each 512-position prompt pass, from the profile, before interleaving:

| Part | Time | Share |
|---|---:|---:|
| Experts' products (GPU) | 1.11 s | 66% |
| Attention-phase products and attention (GPU) | 0.35 s | 20% |
| Waiting between submissions | 0.13 s | 8% |
| CPU routing, gathering and adding back | 0.11 s | 7% |

The expert products ran at about 2.2 TFLOPS of f32. Alone, the tiled kernels reach
3.5-4.4 TFLOPS on the expert shape (`metal-compute/tests/gemm_timing.rs`,
`time_tiled_products_on_expert_shapes`). In a pass, the 2,048 routed positions spread
unevenly over 32 experts (18 to 161 each), and padding each to a multiple of 32 adds
about 25% (2,544 padded rows).

Interleaving two passes (`pass_pair`) took the 4,246-token prompt from 15.1-16.7 s to
12.6-13.0 s (327-338 tokens/s). With two passes in flight the GPU overlaps one pass's
experts with the other's attention: a pass now takes 1.40-1.44 s of wall time, less
than the 1.51 s of GPU time it took alone. Per-submission GPU times in the profile
overlap now and no longer add up to the wall time. Running a layer's experts
concurrently, without interleaving, changed nothing (1.12 s per pass): the GPU was
already busy within a pass.

A 1,810-token chat prompt (summarize this document) read in 5.3 s (342 tokens/s); the
answer was accurate, generated at 48.7 tokens/s. Decoding is unchanged at 57 tokens/s.

## Running it

`~/pudding/run-gpt-oss.sh` (outside any repository, at the workspace root):

- With no text it starts an interactive chat: one message per line, `/reset` to start
  over, `/quit` or Ctrl-D to stop.
- With text it answers once; `--file PATH` puts a file's text before the question;
  `--raw` continues the text instead of answering it.
- `--reasoning`, `--tokens`, `--show-reasoning` and `--profile` pass through; `--cpu`
  uses the reference path.

It builds `gpt_oss_generate` in release and runs it on the GPU with the verified copy
`~/.loadngo/models/<BLAKE3>.gguf`. `--blake3` checks the file's hash beside the load
(9 s instead of about 7 s), and the tool refuses to run a file that does not match.
When the copy is missing and the pudding CAS (Zhoenus II) is attached, the script first
restores it with `archive_cas_restore`, which checks BLAKE3 as it copies.

Checked 2026-10-04:

- "What is the capital of France? One word." -> "Paris";
- a two-turn chat that remembered a name from the first turn;
- `--file docs/ZHOENUS_HEAD_MODEL_RUNNER.md` with a question about it: an accurate
  one-sentence answer;
- `--raw` continuation;
- an unknown option refused in one line.

Each chat turn re-reads the whole conversation: harmony leaves earlier replies'
reasoning out of the history, so the cached positions would not match it.

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

1. **Prompts.** The padding of each expert's positions to 32 costs about 25% of the
   expert products. A tiled kernel with a 16-position tail, or one grouped dispatch
   over all experts, would recover part of it. Half precision would not help much:
   Apple GPUs run f16 and f32 arithmetic at the same rate.
2. **Long-context decoding.** About 6 ms per token is still attention at 4k
   positions. Split each head's keys across threadgroups.
3. **Tools in the chat format.** Declarations in the developer message, and calls on
   the commentary channel to `functions.NAME`.
4. **Load without the double copy.** Repack each tensor as it arrives (peak now
   29.6 GB).
5. **The model service.** `network::model_service` runs this engine instead of
   `llama-server`, keeping its by-hash model resolution.
