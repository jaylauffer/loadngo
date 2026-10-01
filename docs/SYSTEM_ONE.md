# System One: typed, probabilistic decisions from a local model

`loadngo-inference::system_one` (2026-09-27) applies the principles of TypeSafe's
"System One" models (Jev, <https://typesafe.ai/blog/introducing-system-one-models-and-jev>)
to any local model, with no hosted service:

- **Unstructured state in, typed decisions out.** A question has a type:
  - `noul`: true or false;
  - `choice`: one of up to 26 labelled options;
  - `score`: an ordinal scale.

  The answer is a probability for every allowed option and never free text, so it
  cannot fall outside its type.
- **All options in one pass.** Options are shown as letters `A`, `B`, ..., each a
  single token. One forward pass gives the model's next-token score for every letter
  at once. Nothing is generated.
- **State read once.** An adapter reads the state once and reuses its session for
  every question about it.
- **Calibrated confidence.** Probabilities are `softmax(logit / T)`. `fit_temperature`
  fits `T` on labelled examples. `calibration_report` gives accuracy, mean confidence,
  expected calibration error, Brier score and NLL. TypeSafe trains for calibration
  (RLCD); we cannot retrain a 48B model here, so temperature scaling is the post-hoc
  equivalent, fitted on data from our own tasks.
- **Code controls.** The decision picks among options; deterministic code verifies
  and acts. Low confidence goes to a person (see `CAS_DRIVE_CLEANUP.md`).

The request and response shapes match TypeSafe's API, so the two are interchangeable:

```json
{"state": "...", "questions": {
  "action": {"type": "choice", "instructions": "...", "criteria": {"keep": "...", "remove": "..."}},
  "urgent": {"type": "noul", "instructions": "..."},
  "risk":   {"type": "score", "criteria": {"0": "...", "1": "...", "2": "..."}}}}
```

The response is `{"answers": {id: {label: probability}}}`.

## Kimi as the model

kimi-k3-in-rust `--system-one FILE [--temperature T]` answers a request with Kimi
Linear. The state goes in a user message after a short system instruction, and each
question's letters are read from the logits of the assistant's first token.

First run, 2026-09-27 (Mac mini, `--accel gpu`). The state described a 25 GB Cargo
`target/` directory:

| Question | Answer |
|---|---|
| choice: keep / archive then remove / remove | remove 99.4% |
| noul: everything here can be recreated from source | true 98.3% |
| score 0-3: risk of deleting it | 0 ("no risk") 79%, 1 ("minor inconvenience") 20% |
| noul (control): the text says the sky is green | false 98.9% |

It took 17.3 s for four questions: 99 state tokens read once, then 200 question
tokens. The time is Kimi's prompt processing, which is still CPU-bound
(`METAL_COMPUTE_PLAN.md`, "M1, second stage"). TypeSafe quotes 70-500 ms for Jev.

## Checkpoints and the Task model

Since 2026-10-02 Kimi's chat asks System One where a turn stands each time her
context is rebuilt from a handoff (kimi-k3-in-rust `docs/CHAT.md`, "Checkpoint"). The
questions were chosen to line up with the Task work protocol
([TASK_OFFER_PROTOCOL.md](TASK_OFFER_PROTOCOL.md), `data/src/p2pmsg.rs`), because a
long chat turn and a Task assignment are the same thing: a request, a worker reporting
on it, a claim that it is done, and the requester's verdict.

| Task message | Field | In Kimi's chat | System One |
|---|---|---|---|
| `TaskRequest` / `TaskAccept` | `summary`, `success_criteria` | Jay's message, word for word | first half of the state |
| `TaskStatus` | `note` | the handoff she writes | second half of the state |
| `TaskStatus` | `state` (a free string: the code sends `"running"` and `"in-progress"`) | none | `state`: choice of `in-progress`, `blocked`, `complete` |
| `TaskResult` | the claim that the criteria are met | her final answer | `state` = `complete`; `progress` = 3 |
| `TaskAck` | `accepted` | Jay's verdict | the label the answers are measured against |

What follows from putting the two together:

- **`TaskStatus.state` can be a type.** Today each worker writes what it likes. A
  choice gives the submitter values it can compare across workers, each with a
  probability.
- **The submitter should ask, not the worker.** The protocol gives selection, timeout
  policy and verification to the submitter. A checkpoint is a submitter-side decision
  over `(TaskRequest, TaskStatus)`: keep waiting, ask, or reassign. In the chat the
  same model both writes the notes and judges them, so its errors are correlated;
  asked by the submitter's own model about another node's status, they are not.
- **`TaskAck` is the calibration data this document says is missing.** Every closed
  assignment gives a state text, an answer distribution and `accepted`. Those are
  `Example`s for `fit_temperature` and `calibration_report`. A question leaves shadow
  mode when its report on real acknowledgements is good enough to set a threshold, and
  not before.
- **An answer never stands in for `TaskAck`.** The qcoin receipt follows accepted work
  that is anchored. A probability may decide whether to look, or what to look at
  first. Acceptance stays with the submitter's verification of the success criteria.

Built: the chat checkpoint, in shadow mode (answers shown and saved, nothing acts on
them), with the field names above. Not built: anything in the Task runtime. The
recommendations for it, with the first run's results, are in
[TASK_CHECKPOINT_RECOMMENDATIONS.md](TASK_CHECKPOINT_RECOMMENDATIONS.md).

## Still open

- **Calibration on real tasks.** Labelled examples from our own decisions (the
  drive cleanup, voice activation) are needed before the probabilities are trusted.
  The test suite only shows that the fitting works on synthetic data.
- **More than 26 options** (TypeSafe allows 255). This needs multi-token labels or a
  two-level choice.
- **Speed.** Faster prompt processing, and a small decision model on the Neural Engine.
