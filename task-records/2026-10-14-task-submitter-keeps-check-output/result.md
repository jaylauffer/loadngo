# Result: task_submitter keeps what its check printed

Worker: claude-code-macmini (Claude Code, Opus 5.5, in Jay's session). Task request
16234815405030951388, assignment 14453840850501999899; intent and check in loadngo 1ba558d2.

commit: eb8d25f1e0103880d240bc0a35933162741a4ea2

## What changed

- `network/src/task_runtime.rs`: `Verification` (command, exit code, the last 64 KiB of
  stdout and of stderr from a character boundary, bytes dropped) and its rejection note
  (exit and the check's last line, stderr first, 200 characters at most).
- `network/src/bin/task_submitter.rs`: the closure record is written for every result
  checked, with `accepted` and `verification`, and no receipt when rejected; each line
  of the output goes to the log as `task_submitter_verify_output`; a rejection's TaskAck
  note is that rejection note. The receipt and its commitment are unchanged.
- Docs: `docs/TASK_REWARD_FLOW.md` (the completion receipt), `task-records/README.md`.

## How it was checked

- New tests: tail at a character boundary and bounded, the rejection note (stderr
  first, stdout, signal, long line), a rejected closure record. fmt, strict clippy and
  all network tests pass on macOS.
- The check itself (verify.sh as pushed in 1ba558d2) was run once before this hand-over,
  on scratch paths: all three parts passed, including both loopback cycles.

## Not done

- The Task binaries still poll their sockets every 25 ms (task-node every 250 ms); that
  is the next Task (Jay asked for it after this one's intent was pushed).
