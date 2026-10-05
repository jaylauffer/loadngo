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
| `state` (then a choice of `in-progress`, `blocked`, `complete`) | `blocked` 53%, `in-progress` 46% | looping on one failing approach |
| `repeating` | false 67% | the same error five times |
| `progress` | 2 "part of the change is made", 93% | nothing was changed (1) |

Two things follow, and they are the basis of every recommendation below:

1. **A worker's own note understates its failures.** It is written by the party with
   the least distance from the work.
2. **A judge that reads only that note inherits its errors, and is confident about
   them.** The 93% was the worst answer of the three.

## Recommendations

### 1. Two lifecycles, each with a closed set of states

Revised 2026-10-02 after Jay's questions: the first draft had one set of three values
(`in-progress`, `blocked`, `complete`). It left out a task nobody holds, and `blocked`
covered three situations that need three different responses.

There are two things with a status, held by different parties.

**The task**, held by the submitter. `TaskStatus` cannot report it: before
`TaskAccept` there is no worker to send one. Today the submitter keeps this implicitly
in its control flow.

| Task state | Meaning | Becomes |
|---|---|---|
| `open` | requested, no worker holds it (unclaimed) | `assigned` on `TaskAccept`; `expired` |
| `assigned` | one worker holds it; see the assignment's state | `submitted`; back to `open` if the worker withdraws or is timed out |
| `submitted` | a `TaskResult` is waiting for verification | `accepted`; back to `open` on `TaskAck(accepted=false)` |
| `accepted` | verified and acknowledged (any reward is settled separately) | final |
| `expired` / `cancelled` | nobody took it in time, or the submitter withdrew it | final |

A task returns to `open` more than once in its life. What the earlier worker did
(its last status note and artifacts) should travel with the reopened request, so the
next worker does not start from nothing.

**The assignment**, reported by the worker in `TaskStatus.state`. `state` is a
`String` today: `task-node` and `task_worker` send `"running"`, the tests and codec
example use `"in-progress"`. Recommend these values:

| Assignment state | Meaning | Who acts next | Submitter's response |
|---|---|---|---|
| `in-progress` | working; the next step differs from what failed | worker | wait for the next status |
| `paused` | stopped for a reason unrelated to the task (budget spent, preempted, thermal, operator); will resume by `next_check_in_by` | worker | wait; do not count it against the delivery estimate as a stall |
| `needs-input` | cannot go on without something from the submitter: a decision, an answer, access, an artifact. The note asks the question | submitter | answer |
| `needs-help` | the work needs expertise or capability this worker lacks. The note names it as capability tags and says what is done so far | submitter | bring in another worker (below) |
| `withdrawn` | the worker gives the assignment back; the note says why and what exists | submitter | the task is `open` again |
| `complete` | the worker believes the criteria are met | worker | expect `TaskResult` |

`"running"` reads as `in-progress`. An unknown value is treated as `needs-input`, so
a worker that says something new gets looked at instead of waited on.

`blocked` is gone. Paused, waiting on the submitter and needing another worker have
different owners of the next move, and a submitter that cannot tell them apart can
only guess between waiting, answering and reassigning.

**Stuck is not a reported state.** A worker looping on a failing step does not know
it, or does not say so: Kimi's note called five failures an "initial attempt" and she
would have reported `in-progress`. Stuck is what the submitter concludes from the
counts (recommendation 2) and the typed questions (recommendation 3). The reported
states are what the worker knows; the judged ones are what the submitter infers.

#### Needing someone else, discovered during the work

A `TaskOffer` is made from the request's summary and capability tags, before the work
is understood. That a task needs other expertise often shows only partway through. So
`needs-help` is an ordinary outcome, not a failure of the offer, and it must be
reportable at any point after `TaskAccept` without penalty beyond the unfinished
work.

What the status carries: the capability tags the worker found it lacks, and a
handoff of what exists so far (done, facts established, what failed, artifacts). The
handoff Kimi writes at a context compaction has this shape for the same reason: it is
written for whoever continues, and that may be a different worker.

What happens next is the submitter's choice, because the protocol gives the submitter
selection and the reward:

- **A helper**: a second `TaskRequest` for the missing part, with those capability
  tags and the first worker's handoff as its artifact. The first assignment stays
  open (`needs-help` until the helper's result arrives, then `in-progress`). Two
  assignments, two acknowledgements.
- **A replacement**: the first worker's assignment ends as `withdrawn` and the task
  reopens with the wider capability tags and the handoff.

Not recommended yet: the worker issuing its own `TaskRequest` for the missing part and
answering for the helper's result. Any node may be a submitter, so the wire allows it,
but it puts verification and reward for the sub-task with a party the original
submitter did not choose. Left open.

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
counts first, then its note) to a System One model as the state, and asks `repeating`,
`progress`, and whether the reported `state` is the one the evidence supports
(a choice over the assignment states above plus `stuck`).

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

1. **Which assignment to look at first.** Low confidence, a judged stall, or a note and
   counts that disagree go to the top of the submitter's list. Wrong answers cost
   attention only.
2. **When to ask the worker a question** before the status interval runs out.
3. **When to stop waiting**: reissue the request or self-execute, the actions
   "Timeout And Recovery" already gives the submitter. A wrong "stuck" here stops
   good work, so this needs measured calibration.

### 6. What an answer must never decide

`TaskAck(accepted = true)` is the reward gate: a reward, if one was agreed, is
settled only for accepted work, and accepting the work does not depend on the
reward ([TASK_REWARD_FLOW.md](TASK_REWARD_FLOW.md); the runtime still sets
`accepted` from `verification_ok && qcoin_tx_hint.is_some()` until that plan
lands). `accepted` comes from deterministic verification of the success
criteria. A probability does not replace that, at any confidence. It may
order the verification queue. It may not shorten it.

## Order of work

| Step | Change | Where | Needs |
|---|---|---|---|
| 1 | Assignment states documented and sent by workers; task states named in the submitter | `TASK_OFFER_PROTOCOL.md`, `task-node`, `task_worker` | nothing |
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
