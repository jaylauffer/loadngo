# Result: the model service runs on loadngo inference (second run)

Worker: claude-code-macmini (Claude Code, Opus 5.5, in Jay's session). The first run
(intent e935c46c, rejected 843ec9d6) failed only on its own grep; this run's check
(f39ecc78) differs only there.

commit: 85c54a5dd889fcafd8519593a3f4f101632f884b

## What changed (0a0a728a, 85c54a5d)

- `gpt_oss_serve` (loadngo-gpt-oss): holds gpt-oss-20b, answers one JSON line per call
  on `~/.loadngo/node/gpt-oss.sock` (directory 0700, other users' connections closed):
  `chat`; `ok`, `expired`, `busy`, `error`. Accept, read and reply on the proactor; the
  model on one thread with a bounded queue; SIGTERM/SIGINT through a pipe the proactor
  waits on, socket removed.
- `data::model_store`: the model by content (restored from the Archive CAS once, hashed
  beside the load at every launch, a swapped copy refused), with its four tests.
- `loadngo_gpt_oss::engine` (moved from gpt_oss_generate) with `answer()`;
  `loadngo_gpt_oss::call`.
- Removed: `network::model_service`, `zhoenus_head_model`,
  `docs/ZHOENUS_HEAD_MODEL_RUNNER.md`. Docs: GPT_OSS.md "Serving", README,
  LOADNGO_NODE.md, DECISION_MODEL.md.
- 85c54a5d: loadngo-proactor became a dependency on every Unix target (Linux CI could
  not compile the server without it).

## How it was checked before this hand-over

- CI 38468350133 at 85c54a5d passed on all four platforms; Linux clippy and tests also
  run on dolores.
- This run's verify.sh, run once on scratch paths: all four parts passed (answers in
  3.62 s and 2.11 s; 18 context switches idle in 10 s while a 26-minute Unreal build
  loaded every core, 11 when the Mac was quiet). Handed over after that build finished.
