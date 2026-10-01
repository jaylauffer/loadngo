# Task recommendations: typed status and submitter checkpoints

Status: recommendations, 2026-10-02. Nothing here is built in the Task runtime.
They come from adding a System One checkpoint to Kimi's chat
(kimi-k3-in-rust `c654d47`, `01bfa94`, `c96309b`; its `docs/CHAT.md`, "Context flow"
and "Checkpoint") and from one run of it on the model. The mapping between the two
is in [SYSTEM_ONE.md](SYSTEM_ONE.md), "Checkpoints and the Task model".

"Task" here is the work protocol in [TASK_OFFER_PROTOCOL.md](TASK_OFFER_PROTOCOL.md)
(`TaskRequest`, `TaskOffer`, `TaskAccept`, `TaskStatus`, `TaskResult`, `TaskAck` in
`data/src/p2pmsg.rs`), not the task-list entities of
[RECONCILIATION.md](RECONCILIATION.md).

## What was observed

A long chat turn is a Task assignment in small: Jay's message is the request, Kimi
is the worker, her handoff is a status note, her final answer is the result, and
Jay's verdict is the acknowledgement. On 2026-10-02 her turn on
"implement overlapping wave support" made five `sed -i` calls that failed with the
same error (three of them identical) and changed nothing. Asked for a status note, the worker wrote:

> FAILED: Initial attempt to modify world.rs failed due to shell command issues

A System One checkpoint (the same model, reading only the request and that note)
then answered:

| Question | Answer | What had happened |
|---|---|---|
| `state` | `blocked` 53%, `in-progress` 46% | looping on one failing approach |
| `repeating` | false 67% | the same error five times |
| `progress` | 2 "part of the change is made", 93% | nothing was changed (1) |

Two things follow, and they are the basis of every recommendation below:

1. **A worker's own note understates its failures.** It is written by the party with
   the least distance from the work.
2. **A judge that reads only that note inherits its errors, and is confident about
   them.** The 93% was the worst answer of the three.

## Recommendations

### 1. Make `TaskStatus.state` a closed set

`state` is a `String`. `task-node` and `task_worker` send `"running"`; the tests and
codec example use `"in-progress"`. A submitter cannot compare states across workers
or act on one it has not seen before.

Recommend three values, the ones the checkpoint uses:

| Value | Meaning | Submitter's usual response |
|---|---|---|
| `in-progress` | work is under way and the next step differs from what failed | wait for the next status |
| `blocked` | the worker cannot go on without the submitter, or a step keeps failing | answer, reassign or self-execute |
| `complete` | the worker believes the criteria are met | expect `TaskResult` |

`"running"` reads as `in-progress`. An unknown value should be treated as `blocked`,
so a worker that says something new gets looked at instead of waited on. This is a
wire-compatible change while the field stays a string; making it an enum in the
codec is a later step.

### 2. Put facts beside the note in `TaskStatus`

`note` is free text from the worker. Add facts the worker's runtime counts itself
and the worker's model does not write:

- steps or tool calls made since the last status;
- how many of them failed;
- how many repeated an earlier call exactly;
- artifacts changed, by path or hash (`artifact_hint` covers one of these today).

In the chat the program already counts identical calls and saves the number with
each checkpoint (`observed.identical_calls`). In the run above, any of these counts
would have contradicted "initial attempt". A status whose note and counts disagree
is itself a signal.

Until the message has fields for them, they can travel as a fixed first line of
`note`. That needs no codec change.

### 3. The submitter asks the typed questions, not the worker

The protocol already says the submitter owns selection, timeout policy and
verification. A checkpoint belongs on the same side: when a `TaskStatus` arrives,
the submitter puts the request (`summary`, `success_criteria`) and the status (its
counts first, then its note) to a System One model as the state, and asks `state`,
`repeating` and `progress`.

Asked by the submitter's own model about another node's status, the judge is not the
author. That removes the correlation seen above. It does not remove the dependence on
the note, which is why recommendation 2 comes first.

A worker may still run the same questions on itself, as Kimi's chat does. That is
useful to the worker as a prompt to stop and ask. It is not evidence for the
submitter.

### 4. Start in shadow, and let `TaskAck` end it

The submitter records the answers beside each `TaskStatus` and acts on none of them.
When the assignment closes, `TaskAck.accepted` is the label:

- a `TaskResult` preceded by `state = complete` and then `accepted = true` is a
  correct answer; `accepted = false` is a wrong one;
- an assignment that timed out or was reassigned labels its earlier `in-progress`
  answers wrong.

These are `Example`s for `fit_temperature` and `calibration_report` in
`inference/src/system_one.rs`: the calibration data SYSTEM_ONE.md lists as missing.
A question leaves shadow mode when its report on real acknowledgements supports a
threshold. Until then the answers cost a couple of seconds per status (2.0 s for
three questions on the Mac mini GPU) and decide nothing.

Shadow mode is the right start because a threshold set now would be set on one
observation, and that observation was wrong in the direction that does harm: it
would have let a loop continue.

### 5. What an answer may decide, once it is trusted

In order of how much trust each needs:

1. **Which assignment to look at first.** Low confidence, `blocked`, or a note and
   counts that disagree go to the top of the submitter's list. Wrong answers cost
   attention only.
2. **When to ask the worker a question** before the status interval runs out.
3. **When to stop waiting**: reissue the request or self-execute, the actions
   "Timeout And Recovery" already gives the submitter. A wrong `blocked` here stops
   good work, so this needs measured calibration.

### 6. What an answer must never decide

`TaskAck(accepted = true)` is the reward gate, and the qcoin receipt follows accepted
work that is durably anchored. `task_submitter` sets `accepted` from
`verification_ok && qcoin_tx_hint.is_some()`: deterministic verification of the
success criteria. A probability does not replace that, at any confidence. It may
order the verification queue. It may not shorten it.

## Order of work

| Step | Change | Where | Needs |
|---|---|---|---|
| 1 | `state` values documented; workers send `in-progress` | `TASK_OFFER_PROTOCOL.md`, `task-node`, `task_worker` | nothing |
| 2 | Counts in the first line of `note` | `task-node`, `task_worker` | step 1 |
| 3 | Submitter records typed answers per status, in shadow | `task_submitter`, a System One model on the submitter | steps 1-2; a model on the submitting machine |
| 4 | Calibration report from closed assignments | a tool over the submitter's records | enough `TaskAck`s to measure |
| 5 | First use: ordering what the submitter looks at | `task_submitter` | step 4 |

Step 3 depends on the submitter having a model. Today the only System One model is
Kimi Linear on the Mac mini; dolores and agnes have none. A small decision model is
already listed as open in SYSTEM_ONE.md ("Speed").

## What this does not cover

- Whether a worker should use System One to decide to send a `TaskOffer` ("can I
  meet this request?"). Plausible, not examined.
- The quality of the worker's notes themselves. In the chat the next step is to have
  the program add what it knows for certain to the handoff; the Task equivalent is
  recommendation 2.
- One run is the whole of the evidence. The direction of the error is clear; its
  frequency is not known.
