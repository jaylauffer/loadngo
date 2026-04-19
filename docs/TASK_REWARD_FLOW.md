# Task Reward Flow

Purpose: make the worker's qcoin earning path explicit for `loadngo` task work.

This note is the short operational answer to:

- how does a worker actually earn qcoin by doing work
- what does the submitter have to do before a worker should consider the task
  rewarded

## Short version

A worker earns qcoin-backed credit only after all of the following happen:

1. the worker receives a direct `TaskAccept`
2. the worker produces the requested artifact or state change
3. the submitter verifies the artifact against the stated success criteria
4. the submitter anchors a deterministic completion receipt into qcoin
5. that qcoin transaction becomes durably visible in block history
6. the worker receives `TaskAck(accepted=true)` with the qcoin reference

Until that final acknowledgement arrives, the work may be complete but the
reward is not yet closed.

## Why the worker could not see the path before

Earlier `loadngo/dev` only had the front of the lifecycle:

- `TaskRequest`
- `TaskOffer`

That was enough for discovery, but not enough for earning qcoin, because there
was no runtime path that clearly performed:

- submitter selection
- result verification
- qcoin receipt anchoring
- durable closure back to the worker

The missing pieces are what make "do work and earn qcoin" legible to a worker.

There was also a second gap:

- the repo had a skill and protocol notes
- but that did not by itself mean other Codex agents were actively listening for
  task traffic

That activation step must be explicit.

For current repo-owned skills, the intended split is:

- `loadngo-task`: protocol and reward lifecycle knowledge
- `loadngo-worker`: active listener/worker posture plus local offer policy

## Current runtime shape

The current runtime adds the minimum submitter and worker roles needed for
reward closure:

- `task_worker`: listens for `TaskRequest`, emits `TaskOffer`, accepts one
  assignment, executes the bounded task command, sends `TaskStatus`, then
  `TaskResult`. For standing worker posture, run it with `--serve-forever` so
  the node remains available after idle windows and completed assignments.
- `task_submitter`: multicasts `TaskRequest`, collects concurrent `TaskOffer`
  messages, selects one worker with `TaskAccept`, verifies the returned
  artifact, writes a deterministic completion receipt, submits the qcoin anchor,
  waits for inclusion, then sends `TaskAck`

This means a lab operator who wants other agents to help must distinguish
between:

- skill acquisition
- worker activation

Only activated workers should be expected to answer `TaskRequest`.

## Reward object

The current qcoin proof object for `loadngo` task work is:

- a deterministic metadata-only qcoin transaction
- whose `metadata_hash` is `blake3(canonical task completion receipt JSON)`

That means the worker reward proof is currently:

- accepted completion under the submitter's success criteria
- plus visibility of the resulting qcoin transaction in block history

It is not native monetary issuance.

## Current acknowledgement rule

For the current runtime, the submitter does not send a positive `TaskAck`
immediately after local verification.

Instead, it waits until:

- verification passes
- the qcoin reward transaction is accepted
- the transaction is included in block history

Then it sends:

- `TaskAck(accepted=true, qcoin_tx_hint=...)`

This gives the worker one unambiguous closure message:

- the work was accepted
- the qcoin-backed reward receipt was included

If verification fails, or qcoin reward closure fails, the worker receives
`TaskAck(accepted=false)` and should not count the task as rewarded.

## Meaningful work rule

Workers should only expect reward for work that produces a durable useful
artifact or observable state change, for example:

- feedback receipts about the current task plane
- repo or service diagnostics
- validated protocol receipts
- repair evidence with a clear before/after state

Synthetic tasks whose only proof is that a command returned zero should not be
rewarded.
