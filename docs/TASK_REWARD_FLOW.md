# Task Reward Flow

Purpose: how accepted `loadngo` task work earns a reward, and how the reward is
kept separate from accepting the work.

## Decision (Jay, 2026-10-06)

**Accepting the work does not depend on the reward.**

- `TaskAck.accepted` means one thing: the submitter verified the result against
  the stated success criteria.
- A reward is optional. The submitter's operator and the task node's operator each
  decide which reward schemes they take part in, if any. A Task with no reward is
  an ordinary, complete Task.
- QCoin is the first-party reward scheme, not a requirement. A task node must run
  without QCoin.

## Runtime (2026-10-06)

This is built; the sections after it describe the design.

The Task roles:

- `task-node`: a standing worker node on top of `loadngo-proactor`. It stays on
  the task plane, accepts bounded assignments, and keeps assignment state until
  `TaskAck` or timeout.
- `task_worker`: listens for `TaskRequest`, emits `TaskOffer`, accepts one
  assignment, runs the bounded task command, sends `TaskStatus`, then
  `TaskResult`. It is a bounded helper, not the standing worker runtime.
- `task_submitter`: multicasts `TaskRequest`, collects `TaskOffer`s, selects one
  worker with `TaskAccept`, verifies the returned artifact, writes the completion
  receipt, settles the agreed reward if there is one, then sends `TaskAck`.

What the code does:

- **Acceptance is verification alone.** `task_submitter` sets
  `accepted = verification_ok`. A task with no reward, or with a settler that is
  slow or down, still closes with one `TaskAck`.
- **Each operator chooses its schemes.** The submitter offers the schemes given
  with `--reward <scheme>=<command>`; a worker (`task-node`, `task_worker`) offers
  the payees given with `--reward-payee <scheme>=<payee>`, and may check what it is
  paid with `--reward-verify <scheme>=<command>`.
- **Settlement is bounded and offloaded.** The settler runs on a worker thread; the
  submitter waits on a proactor for its answer or a deadline (30 s by default,
  `--reward-settle-seconds`, plus 5 s grace), never polling.
- **loadngo links no scheme.** The shared pieces are in `network::task_reward`
  (matching, the settler contract, running the commands); no QCoin code or type is
  in loadngo, and loadngo no longer depends on qcoin. qcoin depends on loadngo,
  never the reverse.

Checked on a Mac with `task-node` and `task_submitter` on loopback, with test
settler and verifier commands: a rewarded round (`pending`, then `settled` by the
worker's verifier), an unrewarded round, and a settler that exits non-zero
(`accepted = true`, reward `failed`). Unit tests in `network::task_reward` cover
matching, the flags, settler and verifier outcomes, and the deadline.

### Before (until loadngo `87b6f961`)

For the record: `task_submitter` set `accepted: verification_ok &&
qcoin_tx_hint.is_some()`, so an unreachable QCoin meant no `TaskAck` at all; it
paid `blake3(worker_node_id)`, an owner no key controls; it ran
`cargo run -p qcoin-node` from a hard-coded manifest path and polled for
inclusion with `thread::sleep(2s)` for up to 120 s, longer than the worker's 90 s
ack timeout; and `loadngo/network` depended on `qcoin-types` for it, which made
the loadngo <-> qcoin cycle.

## The completion receipt

The scheme-neutral object is the deterministic completion receipt,
`RewardReceipt` in `network/src/task_runtime.rs`: request, offer and assignment
ids, both node ids, summary, success criteria, artifact hint and hash, result
note, timestamps and the agreed reward (scheme and payee). Its commitment is
framed by `loadngo-anchor` under the domain `loadngo.task.reward-receipt`,
version 2 (`reward_receipt_commitment`). The submitter writes it, with the
commitment and the settlement, to `--receipt-path` for every accepted task,
rewarded or not.

A reward scheme settles a receipt. It does not define one. QCoin's settlement is
a metadata-only output to the payee's owner script hash whose `metadata_hash` is
that commitment; it is a proof of accepted work, not monetary issuance.

## Design: pluggable rewards

### Protocol

Message bodies are JSON, so each new field defaults to empty and old peers read
as "no reward".

```rust
TaskRequest { …, reward_offers: Vec<RewardTerms> }   // { scheme: "qcoin", terms: Option<String> }
TaskOffer   { …, reward_payees: Vec<RewardPayee> }   // { scheme: "qcoin", payee: "<scheme-specific payee>" }
TaskAccept  { …, reward: Option<RewardPayee> }       // the scheme and payee agreed for this assignment
TaskAck     { …, accepted: bool,                     // verification only
              reward: Option<RewardSettlement> }     // { scheme, state: settled | pending | failed, reference }
```

- The submitter offers the schemes its operator configured. The worker offers
  the payees its operator configured. Selection matches them; a worker with no
  payees can still be selected for work offered without a reward.
- `TaskAccept.reward` is the binding. It goes into the completion receipt as
  `reward`, so the receipt version is 2 (`REWARD_RECEIPT_VERSION`), as that
  constant's comment requires for any field change.
- `TaskAck.reward` replaces `qcoin_tx_hint`.

### Order of closure

1. The submitter verifies the result. That alone sets `accepted`.
2. If `accepted` is true and a reward was agreed, the submitter runs the
   settler for that scheme. It never runs one for rejected work.
3. The submitter waits up to 30 s for the settlement (Jay, 2026-10-06; an
   operator can lower it), then sends one `TaskAck`: `accepted` as verified,
   and `reward` with whatever state the settlement reached. A slow or
   unavailable settler gives `state: pending` or `failed` with
   `accepted = true`; it never turns accepted work into rejected work, and it
   never stops the `TaskAck` being sent.
4. Nothing more is sent about the reward. A worker given `pending` checks the
   reference itself, later, with its `--reward-verify` command.

The wait has to end well inside the worker's ack timeout (`task-node`
`--ack-timeout-seconds`, default 90 s), or the worker gives up on the
assignment before the `TaskAck` arrives. Today's runtime gets this wrong: the
submitter waits up to 120 s for QCoin inclusion. QCoin makes a block every
5 s (`qcoin-node run --interval-seconds`), so 30 s is six blocks.

### Settlers are external commands chosen by the operator

A settler is a command the operator names per scheme, not code linked into
loadngo:

- `settle` reads the receipt and the agreed payee as JSON on stdin and writes a
  `RewardSettlement` as JSON on stdout.
- `verify` (optional) reads a `RewardSettlement` and reports whether its
  reference is real; a worker uses it to check what it was given, including a
  `pending` reference later on.

Operator configuration, as flags on the existing binaries:

| Binary | Flag | Meaning |
|---|---|---|
| `task_submitter` | `--reward <scheme>=<command>` | offer this scheme and settle it with this command; repeatable; none means no reward is offered |
| `task-node`, `task_worker` | `--reward-payee <scheme>=<payee>` | accept this scheme, paid to this payee; repeatable; none means work unrewarded |
| `task-node`, `task_worker` | `--reward-verify <scheme>=<command>` | optional check of a settlement reference |

A payee is all a worker needs in order to be paid, and it is given when the node is
launched. The task node holds no keys and no balance; spending and balances belong
to a wallet for that scheme, run by the operator apart from the node. For QCoin the
payee is an owner script hash; how an operator makes one, and the stages from
proof-only rewards to a wallet, are in the qcoin repository's
`docs/TASK_REWARDS.md`.

Why a command and not a Rust trait: operators choose at run time without
rebuilding; loadngo links nothing scheme-specific; each scheme's code stays in
its own repository. Settlement happens once per Task, so starting a process
costs nothing that matters.

The submitter runs the settler through bounded worker offload and receives the
result as a proactor completion, not by blocking or polling.

The contract, as `network::task_reward` implements it:

- `settle` reads `{ scheme, payee, receipt, commitment_hex, wait_seconds }` and
  writes `{ scheme, state, reference, note }`. `commitment_hex` is the receipt's
  anchor commitment (domain `loadngo.task.reward-receipt`, version 2), what a
  ledger records, so a settler need not know the framing. It should answer within
  `wait_seconds` with `pending` if the reward is not final; past that plus 5 s
  the submitter kills it and reports `failed`. A non-zero exit, unreadable output,
  or an answer for another scheme is also `failed`.
- `verify` reads a settlement and exits 0 when its reference is real; it may
  write the settlement as it now stands (for example `settled`). The worker logs
  it; the assignment is already closed.
- Commands run through `sh -lc` on Unix and `cmd /C` on Windows.

### QCoin, the first-party settler

`qcoin-node task-reward settle` and `task-reward verify` implement the contract
for QCoin, with the QCoin pieces that used to be in loadngo: building the reward
transaction, its id, and finding it in a block. Operator use is in qcoin
`docs/TASK_REWARDS.md`.

With that code out of loadngo, `loadngo/network` dropped `qcoin-types` and the
loadngo <-> qcoin cycle ended: qcoin depends on loadngo, never the reverse.

### Order of work

| Step | Change | Where | State |
|---|---|---|---|
| 1 | Reward fields in the Task messages; `accepted` set from verification alone | `data/src/p2pmsg.rs`, `task_submitter`, `task-node`, `task_worker`, `task_ack` | done |
| 2 | Settler contract, `--reward` / `--reward-payee` / `--reward-verify`, offloaded settlement; no-reward path tested with no QCoin present | `network` | done |
| 3 | `qcoin-node task-reward settle` / `verify`, moved from `task_runtime.rs` and `task_submitter`; `qcoin-node payee` and the standard payee script (stage 1 of qcoin `docs/TASK_REWARDS.md`) | qcoin | see qcoin `docs/TASK_REWARDS.md` |
| 4 | Remove `qcoin-types` from `network` and the loadngo workspace; update the QCoin-specific statements in the other Task docs | loadngo | done |

### Decided (Jay, 2026-10-06)

- **QCoin payee.** Supplied at launch with `--reward-payee`, no wallet on the
  task node. It is an owner script hash, and QCoin's tooling and docs give each
  task node its own payee; details in qcoin `docs/TASK_REWARDS.md`. The qcoin
  ledger fix that lets a key-locked output be spent landed in qcoin `ed87987`.
- **Wire format.** The messages change in place, with no release that also
  reads `qcoin_tx_hint`. Only loadngo's Task binaries, one test and these docs
  use it (checked across `~/pudding`), every peer is in the lab, and dolores's
  `loadngo-task-node` service is disabled. New fields still default to empty,
  so a peer that omits them reads as unrewarded.
- **Settlement wait.** Up to 30 s, then `TaskAck` with whatever state was
  reached; a `pending` settlement is checked by the worker's `verify`, not
  reported by a later message (closure order above).

### Open

- **Authenticity.** [`TASK_FABRIC_TRUST_MODEL.md`](TASK_FABRIC_TRUST_MODEL.md)
  notes an unauthenticated `TaskAck` is a reward-theft vector. `verify` lets a
  worker check a settlement; it does not authenticate the messages.

## Workers must be activated

A lab operator who wants other agents to help must tell apart:

- skill acquisition: the agent knows the protocol (`loadngo-task`)
- worker activation: the agent is listening and offering (`loadngo-worker`)

Only activated workers should be expected to answer `TaskRequest`.

## Meaningful work rule

Workers should only expect reward for work that produces a durable useful
artifact or observable state change, for example:

- feedback receipts about the current task plane
- repo or service diagnostics
- validated protocol receipts
- repair evidence with a clear before/after state

Synthetic tasks whose only proof is that a command returned zero should not be
rewarded.
