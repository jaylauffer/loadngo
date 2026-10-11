# Decision model: Strands Decider in loadngo

Status, 2026-10-12: the `loadngo-decider` crate answers System One requests with the
published Strands Decider checkpoint on Apple GPUs (`decide` uses the GPU by default on
macOS) or on the CPU. On the CPU its answers match the reference implementation to 4.6e-5;
on the GPU to 1.4e-4, with a 200-token report answered in 0.45 s (CPU 7.3 s, reference
engine on MLX 0.4 s).

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
- `decider/src/gpu.rs` (macOS): the torso on `loadngo-metal-compute` (below).
- `decide` (`cargo run --release -p loadngo-decider --features cli --bin decide`): a request
  on stdin, answers on stdout; `--eval FILE` scores the orchestration cases; `--device cpu`
  runs the reference path.

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

## The GPU path

`decider/src/gpu.rs`, on `loadngo-metal-compute`; the CPU path is its reference.

- **Weights.** Copied once into GPU memory as bfloat16, exactly as the base model's files
  hold them.
- **The adapter is kept apart.** Merged into bfloat16 weights, its small update would be
  rounded with them; the reference's MLX path merges and differs by up to 0.014.
  - Projections that read the same input share one product of their stacked `A`s, padded
    with zero rows to a multiple of 64.
  - Each `scale * B` is padded with zero columns to match.
  - So both run on the matrix units. With a product per `A` and per `B` at their own
    narrow shapes, the adapter took half of a 1,024-position read (2.29 s; 1.25 s
    stacked).
- **Passes.** Reads go in passes of up to 512 positions, one submission for all 24 layers,
  each finishing as a completion on a loadngo proactor.
- **Sessions.** A session's carried state (each DeltaNet layer's convolution history and
  recurrent state, each attention layer's keys and values for the 4,096-token window) is one
  buffer of about 120 MB. Reading each question from a copy of the state's session is a copy
  in shared memory. The decider keeps two sessions between requests.
- **New kernels** in `loadngo-metal-compute`:
  - `delta_rule_recurrence_head_decay` (Qwen's decay is per head, Kimi's per key channel);
  - `rotate_halves_partial` (RoPE on the first quarter of each head);
  - `sigmoid_gate` (attention's output gate).

  Each is tested against its definition in `f64`. The pointer head stays on the CPU: it
  reads a few rows.

### How it is checked (`decider/tests/gpu.rs`, ignored: needs the checkpoint)

| Test | Against | Result (2026-10-12) |
|---|---|---|
| `answers_match_the_reference_engine` | the end-to-end fixture (reference engine, CPU, `f32`) | largest probability difference 1.4e-4 |
| `hidden_states_match_the_cpu_across_passes_and_session_copies` | this crate's CPU path, 1,683 positions over four passes, the last 40 read from a copied session | largest relative (L2) difference per position 3.4e-4; bound 2e-3 |
| the orchestration eval, 127 cases (`decide --eval`) | the reference engine on MLX, same questions (`eval/results-strands-decider-split.json`) | same state class in 127 of 127; class probabilities within 0.009 |

The hidden-state test catches these mutations: no attention output gate; the convolution
history not moved on; a session copy without its key/value caches; the adapter's `B` at
the wrong columns. The eval's attention figure is not comparable with that run: the Python
scorer asked it as a `noul`, the Rust eval asks yes/no in two orders.

## Cost (Mac mini, M4 Pro)

| | CPU (`f32`) | GPU |
|---|---:|---:|
| Load (files in the page cache) | 2 s; 8.5 GB resident | 2.2 s; peak 11 GB while loading (the weights pass through `f32` on the way in) |
| README request (3 questions, 25-token state) | 4.2 s | 0.42 s |
| A 200-token report, 2 questions | 7.3 s | 0.45 s |
| A 1,700-token state, 2 questions | 36 s | 2.5 s |
| The orchestration eval, 127 cases | | 168 s (median 1.2 s per case) |

- The GPU products run at about 2.5 TFLOP/s: 1,024 positions in about 1.1 s without the
  adapter, which is the tiled bfloat16 kernel's speed.
- The reference engine on MLX takes 0.4 s per report and 53 s for the eval.
- The eval asks each case twice (options in both orders), each time reading the state again;
  one request with all four questions would read it once.

## Next

1. **Weights from the CAS by hash**, as `network::model_service` serves gpt-oss, instead of
   the Hugging Face cache; loaded as bfloat16 bytes, not through `f32`.
2. **The node hosts it** (`LOADNGO_NODE.md`). The agent loop's Jev checks then ask it, in
   shadow first, until labels from Jay show the confidence holds.
