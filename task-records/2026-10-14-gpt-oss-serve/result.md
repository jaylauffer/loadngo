# Result: the model service runs on loadngo inference

Worker: claude-code-macmini (Claude Code, Opus 5.5, in Jay's session). Intent and check in
loadngo e935c46c.

commit: 0a0a728ae6ffc8e263aa0baa151d040b6ad21575

## Expected verdict: rejected at criterion 1

The check's "no llama in source" grep includes `*.sh`, and verify.sh itself (the intent,
under task-records/) mentions llama-server, so every commit after e935c46c fails it. The
check cannot be changed: the submitter refuses a verify.sh that differs from e935c46c.
The work is submitted as it is so the rejection is on record; a corrected Task follows
whose only change excludes task-records/ from that grep.

## What changed

- `gpt_oss_serve` holds gpt-oss-20b and answers JSON-line calls on a Unix socket
  (`loadngo_gpt_oss::call`); `data::model_store` keeps the model by content;
  `loadngo_gpt_oss::engine::answer`; `network::model_service`, `zhoenus_head_model` and
  `docs/ZHOENUS_HEAD_MODEL_RUNNER.md` removed. Docs: GPT_OSS.md "Serving".

## How it was checked before this hand-over

- fmt; strict clippy for the whole workspace; tests of data, loadngo-inference,
  loadngo-gpt-oss and network (4 model-store tests).
- The check's own live part (its Python, run against the release server): two answers in
  2.71 s and 1.78 s, expired and errors as specified, 11 context switches idle in 10 s,
  clean SIGTERM.
- CI for 0a0a728a had not finished at hand-over.
