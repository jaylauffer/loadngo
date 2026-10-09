# One chat program for every local model

`loadngo-inference`'s `agent` module (feature `agent`) is the turn loop that runs a local
model's tool calls and checks its work, whatever the model. Started 2026-10-09 at Jay's
request, from the review `~/pudding/reviews/2026-10-09-local-models-claude.md`.

## Why

Until 2026-10-09 there were two chat programs:

- Kimi's, in kimi-k3-in-rust (`kimi-k3-cli/src/chat.rs`, 3,371 lines, with its own
  1,131-line `text_tools.rs`), serving Kimi Linear, Gemma 4 and K3;
- gpt-oss's, in loadngo (`gpt_oss_generate/agent.rs`, 897 lines, on
  `inference::edit_tools` and `work_tools`).

Each fix landed in one of them. gpt-oss got the guards that let it finish a real loadngo
task (verification gate, the chat's own `cargo check`/`test`, bracket check, outlines,
`text_write` shrink guard, undo detection, Jev acting). Kimi did not, because she does
not use loadngo's editing tools. Kimi got the lifecycle (saved transcripts and resume,
context compaction through a handoff, turn budgets with pause and `/continue`, `/undo`,
the repeated-call guard), and gpt-oss did not. A comparison of the two models was also a
comparison of the two programs.

The loop lives in loadngo, written fresh (Jay's 2026-09-16 rule: nothing translated
from kimi-k3-in-rust into this BSD-3 repository); behaviours that came from Kimi's chat
are re-implemented from their description in kimi `docs/CHAT.md`.

## The pieces

| | What | Where |
|---|---|---|
| `Template` | A model's chat format: render a prompt (whole, or appended), read a reply's calls and answer, write tool results, a note from the chat, the start of an answer | per model: `loadngo_gpt_oss::chat::Harmony`; Kimi Linear and Gemma next |
| `Backend` | The model's context: load, feed, generate (every token fed except the one that ends the reply), and a label model for Jev in a side session | per engine: `gpt_oss_generate`'s `GptOss` |
| `Agent` | The turn: after each reply, run tools, send a note, or end; the tools with their guards; Jev's checkpoints and web gate; tool receipts across turns; the chat's own check and board handoff | `inference::agent` |
| `turn` | One user message, blocking, for a terminal | `inference::agent::turn` |
| `Workspace` | The tools and standing instructions (files, editing under `COLLABORATION.md`, `cargo`/`git`, Archive CAS, notes, web) | `inference::agent::workspace` |
| `jev` | Jev's questions (checkpoint, web gate), for any `LabelModel` | `inference::agent::jev` |

The agent does no I/O of its own besides calling tools: a driver feeds it replies and
feeds the backend what it returns. A terminal blocks (`turn`); a GUI can drive the same
agent from host-proactor completions, submitting generation as GPU work and getting the
next step when it completes. Programs the tools run (`cargo`, `git`) and the chat's own
checks wait in a loadngo proactor (`work_tools::run`), with a proactor deadline that
kills an overrun.

## The guards

Mechanical checks first; they need no judging and the transcripts show they matter
more than the model's own view of its work.

| Guard | From | What it does |
|---|---|---|
| Repeated call | Kimi (10-08) | A call with the same name and arguments (JSON-equal) as one that succeeded this turn is not run; its result says so. `cargo`, `git` and terminal tools are exempt. A successful write or other change lets reads be made again; an edit is never replayed. |
| Tools closed | Kimi (10-08) | A second round made only of repeated calls closes the tools: each result asks for an answer, and the reply is begun in the answer itself (`ANSWER_OPENING`). A tool call after that ends the turn. |
| Write failures | Kimi (10-01), retuned | The same write failing twice with the same error, or five writes failing without one succeeding, ends the turn. (Kimi paused at three; gpt-oss's note at three failures in a row now comes first.) |
| Failing tool | gpt-oss (10-04) | Three failures of one tool in a row add a note: change the call or the approach (for `text_edit`, how to copy `old_text`). |
| Undo detection | gpt-oss (10-04) | An edit that puts a file back to a revision it had earlier in the turn adds a note to read the error's lines and change once. |
| Looping reply | Kimi (09-27) | A reply repeating one block (4 copies, at least 64 tokens, block up to 200) is halted and the turn ends. |
| Verification gate | gpt-oss (10-04) | An answer after file changes with no passing `cargo` run since is held back once with a note to run the checks. |
| Chat's own check | gpt-oss (10-04) | When a turn wrote files, the chat runs `cargo check` and `cargo test` in each changed crate and puts the result in the board handoff ("NEEDS JAY" on failure). |
| Archive coverage | gpt-oss (10-04) | An answer after archive searches is held back once with the actual receipts and a note to claim only their scope. |
| Results fit | both | Long results are halved until they fit before the reply's reserve; when nothing fits, each says so; when even that does not fit, the turn ends. |
| Jev checkpoint | gpt-oss (10-04) | Every 6 calls: stuck/repeating nudges, then closes the tools; needs-input asks to put the question to Jay; complete asks to check, then answer. |
| Jev web gate | gpt-oss (10-04) | Before the first approved web call of a turn: is local lookup still missing, given the receipts? |

Each turn's notes also tell the model about the chat itself (`Agent::set_about`): which
model, which engine, its context, its last reply's speed. Kimi could not answer where her
transcripts were or how fast she ran (2026-10-08) because nothing told her. The standing
instructions gained one line against inflated reviews (her 10-08 espeak review called a
55%-ported engine "production-ready"): state what the documents and code say, with their
numbers, and do not rate work above the evidence.

## Status

| Step | | State |
|---|---|---|
| 1 | The loop, its guards and tests; gpt-oss moved onto it | done 2026-10-09 |
| 2 | Kimi Linear and Gemma templates and backend (kimi-k3-in-rust), Kimi on loadngo's editing and `cargo`/`git` tools instead of her own `text_tools`; her terminal tools decided with Jay | next |
| 3 | The lifecycle in the loop: saved transcripts and resume, turn budgets with pause and `/continue`, Ctrl-C pausing a turn, context compaction through a handoff (an appending format's rewrite is a `Rendered::Full`) | after 2 |
| 4 | The evaluation set (`kimi docs/ORCHESTRATION.md`): finished board tasks, every model through the same loop | after 3 |

### Evidence, step 1 (2026-10-09, M4 Pro Mac mini)

- `inference::agent` tests: 18, on a toy format with scripted replies (no model): a
  tool round and the next turn's history, repeated calls and closing, reads after an
  edit, the verification gate once and cleared by a passing `cargo`, both write-failure
  limits and the three-failures note, results cut to fit and a full context, a looping
  reply halted, Jev nudging then closing, the archive note once, each turn's notes
  carrying the receipts and the facts about the chat, the local date. Seven deliberate
  bugs each fail them: no repeat guard, never closing, no gate, no same-write limit, Jev
  never closing, reads never forgotten after an edit, no loop detection.
- `gpt-oss/tests/chat_parity.rs`
  `the_shared_loop_writes_harmony_as_the_template_checked_above` (with the GGUF): the
  template's prompt, call reading, result, note and answer opening equal the functions
  already checked token for token against the model's own chat template.
- Real model, `gpt_oss_generate --gpu --chat`:
  - "What day is it today, and which model and engine are you running on, and how
    fast?": Friday 9 October 2026, gpt-oss-20b on the local Rust engine on the M4 Pro;
    no tool calls. (No speed on a first turn: none has been measured yet.)
  - The seeded task of `GPT_OSS.md` (a `median` wrong for even counts, "find the bug, fix
    it, and show me that the tests pass"): 6 calls (fs_find, fs_list, cargo test,
    fs_read, text_edit, cargo test); Jev's checkpoint "complete 0.75"; the chat's own
    `cargo check` and `cargo test` passed; the handoff on the scratch board listed
    `src/lib.rs`; the diff is the correct fix. The best earlier run took 8 calls.
- Strict Clippy (`--all-targets --all-features -D warnings`) and `fmt` on
  `loadngo-inference` and `loadngo-gpt-oss`; inference without features and with `cas,web`
  (Kimi's) builds; kimi-k3-cli still builds against it. macOS only; CI for the rest.
