# Task records

Tasks run on the Task plane (`task_submitter`, `task_worker`; `docs/TASK_OFFER_PROTOCOL.md`)
whose intent and result are kept in git as proof. One folder per task, named
`<date>-<name>`:

- `verify.sh`: the submitter's success criteria and check, committed and pushed
  **before** the work starts, so they cannot be fitted to the result afterwards.
  `task_submitter --verify-command` runs it; exit 0 is `TaskAck(accepted=true)`.
- After closure: `result.md` (the worker's result, as the submitter copied it; its hash
  is in the receipt), `receipt.json` (the completion receipt and its commitment, from
  `--receipt-path`), `submitter.log` and `worker.log` (every message of the cycle,
  with the request, offer and assignment ids), and copies of evidence the check read
  outside the repo. `task_submitter` keeps only the check's exit status, so a
  `verify-rerun.log` made after closure, marked as a re-run, shows what it prints.

Ad-hoc runs still write to the ignored `task-artifacts/` and `task-receipts/`.
