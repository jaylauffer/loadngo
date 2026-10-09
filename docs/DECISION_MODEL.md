# Decision model: Strands Decider in loadngo

Status, 2026-10-10: the `loadngo-decider` crate answers System One requests on the CPU with
the published Strands Decider checkpoint, and its answers match the reference
implementation to 4.6e-5. A GPU path is next.

## What it is and why

Strands Decider (`strands-labs/strands-decider`, Apache-2.0; weights
`StrandsAgents/strands-decider-2B-hobson-v21`, Apache-2.0, on `Qwen/Qwen3.5-2B-Base`, Apache-2.0)
is a decision model: given a state and typed questions (`noul`, `choice`, `score`), it gives
a probability for every option from one forward pass and generates nothing.

- **The torso** is Qwen3.5-2B, with a rank-16 LoRA adapter. Of its 24 layers, 18 are Gated
  DeltaNet (linear attention with a recurrent state) and 6 are gated softmax attention.
- **The head** replaces the language-model head: a layer norm, then a query from the hidden
  state at `<answer>` and a key from the hidden state at the end of each option's line.
  - Options are scored by what they say, not where they stand, so letter bias does not
    arise.
  - A question may have any number of options.

On the orchestration evaluation (127 finished tasks, `reviews/2026-10-09-orchestration-eval-claude.md`
at the workspace root) it was weak alone but well calibrated. Paired with the rules, it routed
as well as Gemma 4 31B, in about a fortieth of the time and energy:

| | Missed what needed Jay | False alarms | Time per report |
|---|---:|---:|---:|
| Rules + Strands Decider | 0 of 75 | 42 of 52 | 0.4 s (reference, MLX) |
| Rules + Gemma 4 | 0 of 75 | 41 of 52 | 21 s |

So it takes the System One questions the agent loop asks: the Jev checkpoints, the web
gate, and the digest. Those are calls on a loaded model, which the loadngo node will host
(`LOADNGO_NODE.md`).

## How it is built

Written in Rust from the architecture: the reference's own description, the model's
`config.json` and the weights. Its Python is not translated. The reference ran as an oracle,
in a scratch environment, to make the fixtures (`scripts/decider_fixtures.py`, synthetic text
only).

- `decider/src/tokenizer.rs`: the checkpoint's byte-level BPE, with character offsets. The
  BPE core (byte alphabet, ranked merges, Unicode classes) moved to
  `loadngo_inference::bpe`, shared with gpt-oss's tokenizer.
- `decider/src/prompt.rs`: the request rendered as the model was trained to read it, and
  fitted to its 4,096-token window. The questions are reserved room first, up to three
  quarters of the window, and lose their beginning; the state loses its end.
- `decider/src/model.rs`: the torso on the CPU in `f32`, the adapter merged at load. A
  `Session` carries the recurrent and attention state, so a request's state is read once
  and each question continues from a copy.
- `decider/src/head.rs`: the pointer head.
- `Decider` (`decider/src/lib.rs`) puts them together. It implements
  `loadngo_inference::system_one::Decide`, which the agent loop's Jev checks
  (`agent::jev`) and the evaluation (`agent::eval`) now take. A chat model takes part through
  `LetterReadout`, which reads its option letters as before.
- `decide` (`cargo run --release -p loadngo-decider --features cli --bin decide`): a request
  on stdin, answers on stdout; `--eval FILE` scores the orchestration cases.

## How it is checked

| Test | Against | Result |
|---|---|---|
| `tokenizer_parity` (ignored; needs the checkpoint) | Hugging Face `tokenizers`, 23 texts (accents, decomposed marks, Thai, CJK, emoji, added tokens) | every id and offset |
| `prompt_parity` (ignored) | the reference engine's rendering and fitting: 8 requests, three of them too long for the window | every id and option position |
| `tiny_oracle` (runs everywhere) | transformers + peft on a random Qwen3.5 at toy size: 8 layers, unequal DeltaNet key/value heads, grouped attention, LoRA on every projection, 70 positions | hidden states within 1.4e-5; resuming a session equals reading whole; head logits |
| `end_to_end` (ignored; ~1 minute) | the reference engine on the CPU in `f32`, 5 requests of every kind | largest probability difference 4.6e-5 (the fixture rounds to 5e-5) |

The tests catch these mutations:

- no `beta` in the delta rule, or no `exp` on the decay;
- a wrong RoPE sign;
- no attention output gate;
- an unscaled LoRA merge;
- a convolution that drops a tap;
- an option scored at its first token instead of its last;
- leaving the `f32` tensors unrounded (below).

## What the port found

- **The checkpoint's tokenizer is not the base model's.** The base model's `tokenizer.json`
  pre-splits with `\p{M}` in two classes. The decider's file, the one it was trained and is
  served with, keeps Qwen2's older pattern. The vocabulary and merges are the same.
- **bfloat16, including tensors stored in `f32`.** The reference loads the torso in bfloat16.
  Each DeltaNet layer's `A_log` and gated-norm weight, stored in `f32`, are therefore rounded
  to bfloat16 before use, then upcast on the CPU. The model was trained that way too.
  - Keeping their `f32` values moved probabilities by up to 1.8e-3.
  - The gated norm divides by `sqrt(mean(x^2) + 1e-6)`, so small differences in its weight
    show directly.
  - `Model::load(.., bf16)` rounds them, using `loadngo_weights::dtype::round_to_bf16`.
- **Option order matters a little.** Reordering a choice's options moves its answer by up to
  a few hundredths. The README example, read in key-sorted order, gives retail 0.083
  instead of 0.054.
  - serde_json here sorts object keys.
  - `Request::from_text` therefore keeps the document's order for questions and options;
    `from_json` cannot.

## Cost on the CPU (Mac mini, M4 Pro)

| | |
|---|---|
| Load (files in the page cache) | 2 s; 8.5 GB resident (the torso widened to `f32`) |
| README request (3 questions, 25-token state) | 4.2 s |
| A 200-token report, 2 questions | 7.3 s |
| A 1,700-token state, 2 questions | 36 s |

The reference engine on MLX takes 0.4 s per report. The CPU path is the reference that the
GPU path will be checked against, not the way to serve it.

## Next

1. **GPU path** on `loadngo-metal-compute`.
   - Most kernels exist from Kimi Linear's KDA work: `gemm_bf16` for the projections,
     `delta_rule_recurrence` (a per-channel decay; Qwen's is per head, so it is broadcast),
     `causal_conv_silu`, `l2norm_rows`, `rmsnorm_gated_rows`, `attention_grouped`.
   - New: partial RoPE, attention's output gate, and the pointer head.
   - The adapter merges into bfloat16 weights there; the reference's MLX path does the same
     and differs by up to 0.014.
   - Checked against this crate on the end-to-end requests.
2. **Weights from the CAS by hash**, as `network::model_service` serves gpt-oss, instead of
   the Hugging Face cache.
3. **The node hosts it** (`LOADNGO_NODE.md`). The agent loop's Jev checks then ask it, in
   shadow first, until labels from Jay show the confidence holds.
