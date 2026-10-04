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

## Tools in chat

The chat has loadngo's tools (`loadngo-inference`), as Kimi has them:

- `fs_list`, `fs_read`, `fs_find`, `fs_grep`: the local drive, read-only, relative paths
  from `--base` (the launcher passes `~/pudding`);
- `cas_archives`, `cas_list`, `cas_find`, `cas_read`, `cas_grep`: every Archive CAS
  root on the attached drives (plus `--cas-root`). Signatures are checked against
  `--cas-key`; the launcher passes `~/.loadngo/keys/jay-macmini.dilithium2.pub`;
- `memory_save`, `memory_search`, `memory_list`, `memory_forget`: notes in
  `~/.loadngo/gpt-oss/memory.jsonl`. The newest 4 KB of notes open each conversation;
- `web_search`, `web_fetch` (`--no-web` turns them off).

- editing and checking (since 2026-10-04, below): `text_read`, `text_edit`,
  `text_write`, `cargo`, `git`.

Kimi's own editing, terminal and board tools are in kimi-k3-in-rust (Apache 2.0). The
loadngo editing tools are separate, written fresh.

The format is the template's (`chat`):

- **Declaration.** `tool_namespace` renders the tools' JSON schemas as the
  `functions` namespace in the developer message, and the system message gains the
  line that tool calls go to the commentary channel.
- **Calls.** `read_call` finds `to=functions.NAME` in the last header before
  `<|call|>`, whether the model writes it before or after the channel.
- **Results.** `tool_result` writes the result back JSON-encoded, then
  `<|start|>assistant`.

Within a turn, the model's tokens and each result are fed straight on, so the cache
never needs re-reading. Across turns, the history keeps each answer but not the tool
calls that led to it.

Checked (`gpt-oss/tests/chat_parity.rs`, fixture `scripts/gpt_oss_tools_fixture.py`):

- a prompt declaring tools matches the template rendered by Jinja2 (with transformers'
  `tojson`) and tokenized by Hugging Face, token for token;
- the template's call parses back to its tool and arguments, as does a call in the
  order the model writes it;
- the template's result message is exactly `tool_result`'s, for text with quotes,
  newlines, tabs, a backslash and accents.

On the real model, 2026-10-04:

- **Archives.** "Which Archive CAS archives are attached?": its first call passed an
  argument `cas_archives` does not take; the tool refused it, the model called again
  correctly, and it named both attached archives with their real titles, signers and
  dates.
- **Files.** A crate description came back from `fs_read`, quoted exactly.
- **Notes.** One run saved a note with `memory_save`, and the next run answered from
  it.

### Field report: "What day is it today?" (Jay, 2026-10-04)

Jay's first interactive session with the tools, at 06:07 on Sunday 4 October (+07):

```text
> What day is it today, what's the year and month as well?
1302 prompt tokens in 3.96s (329 tokens/s)
67 tokens in 1.25s (53.6 tokens/s)
[tool] web_search {"query":"current date today"} -> 1758 characters
[tool] web_fetch {"url":"https://todaydateandtime.com/today/"} -> 70 characters
[tool] web_fetch {"url":"https://www.timeanddate.com/"} -> 63 characters
[tool] web_fetch {"url":"https://worldtimeapi.org/api/timezone/Etc/UTC"} -> 186 characters
[tool] web_fetch {"url":"https://todaysdateandtime.com/today/"} -> 6188 characters
[tool] web_fetch {"url":"https://todaysdateandtime.com/today/","start":6067} -> 418 characters
[tool] web_fetch {"url":"http://worldtimeapi.org/api/timezone/Etc/UTC"} -> 205 characters
87 tokens in 2.05s (42.4 tokens/s)
Today is **Wednesday, October 3, 2026**.
The current year is **2026** and the month is **October**.
```

(Timing lines between the calls are left out.) Two faults:

- **Ours.** `gpt_oss_generate` computed the system message's "Current date" in UTC.
  At 06:07 +07 that was still Saturday 3 October, so the model was told 2026-10-03.
- **The model's.** Despite having a date, it made 7 web calls (each query left this
  Mac) for something it had been told, and still got the weekday wrong: 3 October 2026
  was a Saturday, not a Wednesday.

Fixed (`now()` in `gpt_oss_generate`):

- The date is local, from `localtime_r`'s offset on Unix (UTC elsewhere, and so
  labelled).
- Every turn, the developer instructions open with the weekday, date, time and offset
  ("It is now Sunday, 4 October 2026, 06:10 local time (UTC+07:00) ...") and tell the
  model to answer date and time questions from that, without looking them up.

The same question through `run-gpt-oss.sh` after the fix: "It's Sunday, 4 October 2026
... Month: October, Year: 2026", with no tool calls.

## Editing, checking and Jev (2026-10-04)

Jay asked for editing, for Jev to be used where possible, for the agent to complete
meaningful tasks, and for local content to come before web searches.

### The tools (`loadngo-inference`, feature `work`)

- **`text_read`, `text_edit`, `text_write`** (`edit_tools`).
  - Edits are UTF-8 text inside the workspace (`--base`).
  - `text_read` shows numbered lines and the file's revision (a BLAKE3 prefix).
  - `text_edit` replaces text that appears exactly once, so it needs no prior read. A
    `revision`, when given in any form (a model may pass the whole header), must still
    be current.
  - `text_write` creates a file, or replaces a whole file the model has read.
  - A `text_read` gutter copied into `old_text` is taken off, line by line: a model
    may copy some lines with their `  574|` and type others without.
  - **`text_format`** runs `rustfmt` (the edition from the nearest `Cargo.toml`) on a
    Rust file the session wrote, so `cargo fmt --check` passes without the model
    re-typing whitespace.
- **Refused edits**, under `COLLABORATION.md`:
  - paths outside the workspace, symbolic links, `..`;
  - `.git`, build output (`target`, `node_modules`, `CACHEDIR.TAG` directories),
    `.loadngo`, files a repository ignores, and secrets;
  - the root's `AGENTS.md`, `CLAUDE.md`, `COLLABORATION.md` and `AGENT-BOARD.md`;
  - a file with uncommitted changes the session did not make;
  - a path another agent holds on the board.
- **Board claims** (`board`). The first write in a repository claims it on
  `AGENT-BOARD.md` as `gpt-oss`, listing the files written. When the chat ends the
  claim becomes a handoff with the files left uncommitted, the request, and the last
  answer. Nothing is committed or pushed; Jay reviews.
- **`cargo`** runs `check`, `test`, `clippy`, `build`, or `fmt --check` in a directory
  with a `Cargo.toml`. **`git`** runs `status`, `diff`, `log` or `show`.
  - Neither uses a shell. Arguments that reach elsewhere or run other programs are
    refused: `--manifest-path`, `--config`, `--target-dir`, `-Z`, git's `-c`,
    `--output`, external diff and text conversion.
  - Each child runs under a loadngo proactor (`work_tools::run`): reader threads post
    the output as completed jobs, and a proactor deadline kills a child that overruns
    (20 min for cargo, 60 s for git). The end of each stream comes back with the exit
    status.

### Jev in the tool loop

System One (`inference::system_one`) answers typed questions with a probability per
option. gpt-oss is its `LabelModel`:

- The questions run in a side session of the same GPU model (4,096 positions), so the
  conversation's cache is untouched.
- The state is read once. Each question is read after it, then the session goes back to
  the state's end (`GpuSession::truncate`, tested against transformers).
- The next token's scores over `A`, `B`, … are the answer.

Two uses (`gpt_oss_generate/jev.rs`, `agent.rs`):

- **Checkpoints.** Every 6 tool calls, over Jay's request and a one-line summary of each
  call:
  - `state` is one of `in-progress`, `needs-input`, `complete`, `stuck`;
  - `repeating` is a true/false question.

  The answers act, rather than shadowing as in Kimi's chat:
  - stuck at 0.5 or more, or repeating at 0.7 or more, puts a note on the next tool
    result to change approach or answer;
  - the second time, the tools close for the turn and the model is told to answer with
    what it has;
  - `needs-input` at 0.6 or more asks it to put the question to Jay; `complete` at 0.7
    or more asks it to check its changes, then answer.
- **The web gate.** Before a web search or fetch, while no local tool has been tried in
  the turn, Jev judges whether the request is about Jay's projects, files, archives,
  notes or this machine. At 0.5 or more the call is not sent, and the model is told to
  look in the workspace, the archives or its notes first.

Prefer local content: the instructions also say so, and they lay out the method: find,
`text_read`, `text_edit`, check with `cargo` and `git diff`, then report what changed and
how it was checked.

### Checks the chat makes itself

The model's own report is not trusted (see the real-task runs below):

- **Failed calls are shown.** The first 220 characters of every error result go to
  stderr, so a failing run can be read afterwards.
- **A failing tool, three times in a row,** gets a note on its result to read the file
  again or try another way.
- **The verification gate.** When the model answers after changing files with no
  `cargo` run that succeeded since, the answer is held back once. The chat ends it as a
  message and adds a user message: run `cargo test`, `clippy` and `fmt --check` in the
  crate you changed, fix what fails, and do not say a check passed unless it ran.
- **An independent check.** When a turn that changed files ends, the chat runs
  `cargo check` and `cargo test` itself in each crate it changed (the nearest directory
  with a `Cargo.toml`), prints `[verify]` lines, and puts the result in the board
  handoff: "Checked by the chat: … passes", or "NEEDS JAY: … FAILS".

`--no-edit` and `--no-jev` turn these off. `--context` now defaults to 32,768.

### Evidence: a seeded task

The setup: a scratch workspace with a board and a crate whose `median` is wrong for
even counts, so its test fails. The request: "In the demo crate, cargo test fails. Find
the bug, fix it, and show me that the tests pass."

- **First run.** The model found and fixed the bug, ran `cargo check` and `cargo test`,
  and handed off on the board. It needed 13 calls, because `text_edit` then refused
  edits without a prior `text_read`, and it passed the whole `text_read` header as the
  revision; it fell back to `text_write`. The checkpoint saw the loop forming (repeating
  0.48).
- **The fix.** Unique `old_text` is enough for an edit, and a revision is read from any
  text.
- **Second run.** 8 calls: list, find, read, one `text_edit`, `cargo check`, a checkpoint
  (in progress 0.86, repeating 0.26), `cargo test`, then the answer.
- **Checked by hand both times:** the tests pass, the diff is only the fix, and the board
  holds gpt-oss's handoff listing `src/lib.rs` as uncommitted.

### Evidence: a real task in loadngo

The request, run through `run-gpt-oss.sh --no-web`: "In loadngo's line editor
(loadngo/line-editor), Alt-D should delete the word after the cursor, the way Ctrl-W
deletes the word before it. Add it with a unit test, then run that crate's tests and
clippy, and tell me what you changed." Each run was reverted afterwards, and its board
row removed; the diffs are kept with the session's evidence.

- **Run 1.** The library change was right, but the test went into a new file, did not
  test the Alt-D decoding, and failed `cargo fmt --check`. Added: `text_format`; the
  instructions name the existing tests module, `fmt --check`, and `fs_grep` (contents)
  against `fs_find` (names).
- **Run 2.** Adding the test failed: the model copied `text_read`'s line numbers on some
  lines of `old_text` and not others. It said honestly that it could not finish. Added:
  line-by-line gutter removal, and a note after three failures of one tool in a row.
- **Run 3.** It added the key mapping but not the enum variant, so the crate did not
  compile, ran no `cargo` command, and answered that the change "compiles and passes
  clippy". This is the failure that matters for dispatching work: the model's report
  cannot be taken on trust. Added: the verification gate, the chat's own check and the
  shown errors (above).
- **Run 4.** The machinery worked: the gate held the answer back, the chat's
  `cargo check` failed, and the board handoff read "NEEDS JAY: … FAILS"; the model
  answered "I'm unable to resolve the compilation errors". The work itself failed:
  - it never saw the existing `apply`, so it wrote a second, 120-line `apply_key`;
  - it searched for the tests module with `fs_find` and a mistyped `#[cfg(test]`;
  - it flipped one closing brace in and out three times, running `cargo test` between;
  - a revision it read before its own earlier edits was refused as stale;
  - Jev's checkpoints never flagged it (stuck at most 0.26, repeating 0.55).

  Added:
  - **A bracket check** (`inference::rust_text`). A Rust edit or write that leaves
    brackets unbalanced, in a file that balanced before, is refused with the line
    numbers. The reader skips strings, raw strings, characters, lifetimes and nested
    comments, and finds every Rust file in loadngo, kimi, sng-roguelite, sng-mahjong and
    qcoin balanced.
  - **An outline.** A partial `fs_read` or `text_read` of a `.rs` file ends with the
    file's items and their lines (functions, `impl` blocks, modules, `#[cfg(test)]`).
  - **Own revisions.** A revision made stale only by the session's own writes is
    accepted.
  - **Undo detection.** An edit that puts a file back to a revision it had earlier in
    the turn gets a note to stop going back and forth and read the error's lines.
    Undoing is mechanical to see, and Jev did not see it.
  - An empty `path` for the fs tools is the workspace (the model wrote `""` three
    times).
- **Run 5**, with those fixes, got much further; a power cut ended it before the turn
  finished (its diff is in pudding `reviews/2026-10-04-gpt-oss-evidence/`).
  - It found the code and the tests module from the outline, with no searching for it.
  - It wrote the enum variant, the Alt-D decoding, the deletion, and a test in the
    existing tests module.
  - The bracket check refused one edit that would have left an extra `}`; the file never
    broke.
  - Two mistakes remained:
    - it pasted the new match arm twice;
    - its test expected two Left arrows to move two words, not two characters.
  - The gate held back its first answer. The undo note fired twice.
  - Jev's checkpoints rose toward stuck (0.35) but stayed under the threshold.

- **Run 6** stopped by hand. The model meant to add a test, and `text_write` replaced the
  600-line `lib.rs` with the 8-line test (the session had edited the file, so the write
  was allowed). It then tried `git show HEAD:src/lib.rs` to recover. Added: `text_write`
  refuses to replace a file of 40 lines or more with under half of them, and points to
  `text_edit`. `text_edit` now takes `line_start`/`line_count`, which the model kept
  passing; they choose among several matches.
- **Run 7 completed the task**: 22 tool calls, the 4-hunk diff in pudding
  `reviews/2026-10-04-gpt-oss-evidence/`. It added the variant (documented as Alt-D),
  mapped `ESC d`, and implemented the deletion with the existing `word_end` (the span Alt-F
  moves over). It added an assertion to the existing word-editing test. It ran
  `cargo test` four times while it settled what the test should expect, then
  `cargo clippy`, and reported accurately.
  - Checked by hand: `cargo test` 5 pass, `clippy --all-targets --all-features -D warnings`
    and `fmt --check` are clean, and the diff is minimal and correct.
  - The chat's own check did not run: it ran only when the model's last `cargo` run had
    not passed. It now runs whenever a turn wrote files.
  - The change is left uncommitted for Jay, as the handoff says.

## Line editing

`loadngo-line-editor` (`line-editor/`) gives the interactive chat line editing:

- moving: arrow keys, Home/End and Ctrl-A/E;
- words: Alt or Ctrl with an arrow, Alt-B/F, Ctrl-W;
- deleting: Ctrl-U, Ctrl-K, Delete;
- history: Up/Down step through the lines entered this session;

On a Mac, Alt is the Option key, and the letter is lowercase (Option-Shift-D sends a
capital `D`, which nothing uses). An Alt key reaches the editor as Escape followed by the
letter, so on any terminal pressing Escape, letting go, then `d` does the same. Terminal
and iTerm2 type `∂` for Option-D unless Option is set to act as Meta:
- Terminal: Settings → Profiles → Keyboard → "Use Option as Meta key";
- iTerm2: Settings → Profiles → Keys → Left Option key: Esc+.

Option with the left or right arrow moves by word without that setting.
- Ctrl-C abandons the line; Ctrl-D on an empty line ends input.

On a Unix terminal it enters raw mode for each line and restores the settings when the
line ends. Elsewhere (a pipe, a file, or Windows for now) it reads plain lines. The key
decoding and the editing are pure and unit-tested. A run under `script` (a
pseudo-terminal) edited, recalled and ended lines as typed.

That run found a bug, now fixed. Entering raw mode with `TCSAFLUSH` threw away keys
already typed, as when typing ahead while a reply prints; it now uses `TCSANOW`.
Lines wider than the terminal wrap: each redraw starts from the line's first row, places the cursor by row and column (wide characters that do not fit move to the next row, as the terminal moves them), and Enter or Ctrl-C first moves below the whole line. Jay's chat showed the old redraw, which started from the current row and so printed the line again on every key once it wrapped. Checked in a 20-column pseudo-terminal read back through a terminal emulator (pyte): typing, Home, End, insertion, Ctrl-W, wide characters, Enter from the first row, and at the bottom of the screen. A window resized while a line is being edited may still leave stray rows.

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
