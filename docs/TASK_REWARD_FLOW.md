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

The runtime does not do this yet. The section below describes the code as it is.
The plan after it is what changes it.

## Current runtime (as of loadngo `87b6f961`)

The Task roles:

- `task-node`: a standing worker node on top of `loadngo-proactor`. It stays on
  the task plane, accepts bounded assignments, and keeps assignment state until
  `TaskAck` or timeout.
- `task_worker`: listens for `TaskRequest`, emits `TaskOffer`, accepts one
  assignment, runs the bounded task command, sends `TaskStatus`, then
  `TaskResult`. It is a bounded helper, not the standing worker runtime.
- `task_submitter`: multicasts `TaskRequest`, collects `TaskOffer`s, selects one
  worker with `TaskAccept`, verifies the returned artifact, writes a
  deterministic completion receipt, submits it to QCoin, waits for inclusion,
  then sends `TaskAck`.

How QCoin is wired in today, checked in the code:

- **Acceptance requires QCoin.** `task_submitter` sets
  `accepted: verification_ok && qcoin_tx_hint.is_some()`. If QCoin is
  unreachable, `anchor_reward` fails, the submitter exits with an error, and
  no `TaskAck` is sent at all; the worker waits until its ack timeout.
- **The worker has no say in the payee.** The QCoin output's owner is
  `blake3(worker_node_id)` (`reward_owner_hash` in `network/src/task_runtime.rs`),
  not an owner script the worker holds keys for.
- **The task node has no reward configuration.** `task-node` and `task_worker`
  only log `qcoin_tx_hint`.
- **Settlement blocks.** `task_submitter` runs `cargo run -p qcoin-node` for
  `submit-tx`, `tip` and `block`, from a hard-coded manifest path, and polls for
  inclusion with `thread::sleep(2s)`, against the proactor rules in the
  workspace `AGENTS.md`.
- **It ties the repositories together.** `loadngo/network` depends on
  `qcoin-types` only for this code (`reward_transaction`, `tx_id_hex`,
  `block_contains_tx_id` and `task_submitter`), while qcoin depends on
  `loadngo-pq-crypto`, `loadngo-proactor` and `network`. That edge is the
  loadngo <-> qcoin cycle in [`GAME_DEPENDENCIES.md`](GAME_DEPENDENCIES.md).

## The completion receipt

The scheme-neutral object is the deterministic completion receipt,
`RewardReceipt` in `network/src/task_runtime.rs`: request, offer and assignment
ids, both node ids, summary, success criteria, artifact hint and hash, result
note and timestamps. Its commitment is framed by `loadngo-anchor` under the
domain `loadngo.task.reward-receipt`, version 1.

A reward scheme settles a receipt. It does not define one. QCoin's current
settlement is a metadata-only transaction whose `metadata_hash` is that
commitment; it is a proof of accepted work, not monetary issuance.

## Plan: pluggable rewards

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
- `TaskAccept.reward` is the binding. It goes into the completion receipt, so
  the receipt version becomes 2 (`REWARD_RECEIPT_VERSION`), as that constant's
  comment requires for any field change.
- `TaskAck.reward` replaces `qcoin_tx_hint`.

### Order of closure

1. The submitter verifies the result. That alone sets `accepted`.
2. If `accepted` is true and a reward was agreed, the submitter runs the
   settler for that scheme. It never runs one for rejected work.
3. The submitter waits a bounded time for the settlement, then sends one
   `TaskAck`: `accepted` as verified, and `reward` with whatever state the
   settlement reached. A slow or unavailable settler gives
   `state: pending` or `failed` with `accepted = true`; it never turns accepted
   work into rejected work, and it never stops the `TaskAck` being sent.

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

### QCoin, the first-party settler

`qcoin-node` gains `task-reward settle` and `task-reward verify`. The QCoin
pieces now in loadngo move into qcoin with them: building the reward
transaction, the transaction id, and finding it in a block. The node waits for
inclusion from its own chain view rather than being polled through
`cargo run`.

With that code gone, `loadngo/network` drops `qcoin-types` and the
loadngo <-> qcoin cycle ends: qcoin depends on loadngo, never the reverse.

### Order of work

| Step | Change | Where |
|---|---|---|
| 1 | Reward fields in the Task messages; `accepted` set from verification alone | `data/src/p2pmsg.rs`, `task_submitter`, `task-node`, `task_worker`, `task_ack` |
| 2 | Settler contract, `--reward` / `--reward-payee` / `--reward-verify`, offloaded settlement; no-reward path tested with no QCoin present | `network` |
| 3 | `qcoin-node task-reward settle` / `verify`, moved from `task_runtime.rs` and `task_submitter`; `qcoin-node payee` and the standard payee script (stage 1 of qcoin `docs/TASK_REWARDS.md`) | qcoin |
| 4 | Remove `qcoin-types` from `network` and the loadngo workspace; update the QCoin-specific statements in the other Task docs | loadngo |

### Open

- **QCoin payee.** Decided (Jay, 2026-10-06): supplied at launch with
  `--reward-payee`, no wallet on the task node. The QCoin details and their own open
  questions are in qcoin `docs/TASK_REWARDS.md`. The qcoin ledger fix that lets a
  key-locked output be spent landed in qcoin `ed87987`.
- **Wire compatibility.** Change the messages in place (every peer is in the
  lab, and dolores's `loadngo-task-node` service is disabled), or keep reading
  `qcoin_tx_hint` for one release.
- **Settlement wait.** The bound in step 3 of the closure order, and whether a
  later message should report a `pending` settlement once it settles, or the
  worker's `verify` is enough.
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
