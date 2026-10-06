# Task Coordination Protocol

Purpose: define the `loadngo` task coordination contract for submitters and
workers operating over IPv6 multicast discovery with direct unicast follow-up.

Status: this is the current intended direction for `loadngo/dev`.

It replaces the earlier "multicast the offer itself" bootstrap with a
submitter-driven lifecycle:

1. the submitter multicasts a `TaskRequest`
2. candidate workers reply directly with `TaskOffer`
3. the submitter selects one worker with `TaskAccept`
4. the worker maintains direct `TaskStatus` updates
5. the worker submits `TaskResult`
6. the submitter closes the record with `TaskAck`
7. a reward, if one was agreed, is settled only for accepted work; accepting the
   work never depends on the reward ([TASK_REWARD_FLOW.md](TASK_REWARD_FLOW.md))

## Core Rules

`TaskRequest` is the only discovery-plane task message that should be multicast.

Everything after discovery is unicast:

- `TaskOffer`
- `TaskAccept`
- `TaskStatus`
- `TaskResult`
- `TaskAck`

The submitter owns task selection and timeout policy.

Workers do not assume ownership just because they saw a request or sent an
offer. Ownership begins only after a direct `TaskAccept`.

Workers may be full Codex agents or narrower service nodes. The protocol should
care about verifiable outputs and correlation, not about whether the worker can
run an LLM locally.

Activation rule:

- a repo skill or local instructions alone do not place a Codex agent onto the
  wire
- a worker only participates when its local user or local runtime explicitly
  activates worker/listener posture

So a submitter should not assume that every machine that "has the skill" is
currently listening for multicast requests.

## Correlation And Concurrency

The protocol uses three identifiers to keep concurrent work straight:

- `request_id`: one multicast request published by one submitter
- `offer_id`: one worker's response to that request
- `assignment_id`: the chosen execution path after the submitter selects a worker

Required behavior:

- every `TaskOffer` must carry the originating `request_id`
- every `TaskAccept` must carry `request_id`, `offer_id`, and `assignment_id`
- every later `TaskStatus`, `TaskResult`, and `TaskAck` must carry the same tuple
- workers must emit at most one live `TaskOffer` per `request_id`
- submitters must tolerate multiple concurrent offers for one `request_id`
- submitters must deduplicate repeated offers by `offer_id`
- only one `TaskAccept` should be considered authoritative for a given `assignment_id`

This is what lets one submitter solicit multiple workers on the same multicast
channel without losing correlation.

## Traffic Shape

The intended execution flow is:

1. `TaskRequest` goes to the IPv6 multicast discovery group
2. candidate workers reply directly to the submitter's reply endpoints with `TaskOffer`
3. the submitter may exchange additional direct details before choosing a worker
4. `TaskAccept` selects one worker and sets execution expectations
5. the selected worker sends periodic `TaskStatus`
6. the selected worker sends `TaskResult` when the success criteria are met
7. the submitter validates the result and responds with `TaskAck`
8. a reward, if one was agreed, is settled only after verification accepts the result

## Message Semantics

### `TaskRequest`

Multicast advertisement from the submitter.

Minimum useful fields:

- `request_id`
- `submitter_node_id`
- `created_at`
- `expires_at`
- `summary`
- `capability_tags`
- `reply_endpoints`
- optional `requested_duration_secs`
- optional `success_criteria`
- optional `artifact_hint`
- optional `note`
- `reward_offers`: the reward schemes the submitter's operator configured, each
  `{ scheme, terms }`; empty offers no reward (and is what a peer that omits it sends)

This message should stay lightweight. It is for discovery and initial matching,
not for shipping large artifacts.

### `TaskOffer`

Direct worker response to one `TaskRequest`.

Minimum useful fields:

- `offer_id`
- `request_id`
- `worker_node_id`
- `created_at`
- `expires_at`
- `capability_tags`
- `reply_endpoints`
- optional `estimated_duration_secs`
- optional `max_status_interval_secs`
- optional `note`
- optional `artifact_hint`
- `reward_payees`: where this worker is paid, `{ scheme, payee }` per scheme its
  operator configured with `--reward-payee`; empty takes only unrewarded work

This is where concurrent candidate workers respond directly to the submitter.

A worker may answer from a constrained machine if it can still perform the
requested task and return a direct verifiable result.

### `TaskAccept`

Direct submitter-to-worker selection and execution terms.

Minimum useful fields:

- `assignment_id`
- `request_id`
- `offer_id`
- `submitter_node_id`
- `worker_node_id`
- `accepted_at`
- `status_check_interval_secs`
- optional `expected_duration_secs`
- optional `expected_delivery_by`
- optional `submitter_reply_endpoint`
- optional `success_criteria`
- optional `artifact_hint`
- optional `note`
- optional `reward`: the `{ scheme, payee }` agreed for this assignment, the first
  scheme in the request's order that the worker has a payee for; absent for unrewarded
  work

This is the authority handoff. It defines the cadence and delivery threshold
that the submitter will enforce.

### `TaskStatus`

Direct worker heartbeat or progress update.

Fields should include:

- `assignment_id`
- `request_id`
- `offer_id`
- `worker_node_id`
- `status_at`
- `state`
- optional `next_check_in_by`
- optional `note`
- optional `artifact_hint`

### `TaskResult`

Direct worker submission that claims the work satisfies the assigned criteria.

Fields should include:

- `assignment_id`
- `request_id`
- `offer_id`
- `worker_node_id`
- `submitted_at`
- optional `artifact_hint`
- optional `note`

### `TaskAck`

Direct submitter closure decision after inspecting the result.

Fields should include:

- `assignment_id`
- `request_id`
- `offer_id`
- `submitter_node_id`
- `acked_at`
- `accepted`: the result met the success criteria; nothing else
- optional `reward`: how far the agreed reward got, `{ scheme, state, reference, note }`
  with `state` one of `settled`, `pending`, `failed`; absent when no reward was agreed
  or the work was rejected (see [TASK_REWARD_FLOW.md](TASK_REWARD_FLOW.md))
- optional `note`

If `accepted` is false, the task remains unclosed from the worker's perspective
and may need resubmission, reassignment, or local execution by the submitter.

## Negotiation

The request does not have to carry every execution detail up front.

The intended pattern is:

- the submitter publishes a bounded `TaskRequest`
- workers reply with direct `TaskOffer`
- the submitter may exchange more direct details before choosing a worker
- the chosen terms are fixed in `TaskAccept`

The values that matter operationally are:

- status check interval
- expected delivery duration
- expected delivery deadline
- success criteria
- artifact references or proof expectations

These belong in the selected assignment path, not in unauthenticated multicast.

## Timeout And Recovery

The submitter is responsible for stale-work handling.

At minimum:

- if no acceptable offer arrives before request expiry, the submitter may reissue the request or self-execute
- if the worker misses the negotiated status interval, the submitter may reissue the request or self-execute
- if the expected delivery threshold is exceeded, the submitter may reissue the request or self-execute
- if a result fails the success criteria, the submitter may reject it with `TaskAck(accepted=false)` and either reopen or self-perform the work

The important point is that timeout policy is attached to the assignment and the
submitter's success criteria, not to multicast visibility alone.

## Anti-Amplification Rules

Required rules:

- workers must never rebroadcast a received `TaskRequest`
- workers must reply only to the submitter's direct endpoints
- submitters must never multicast selection, status, result, or acknowledgement traffic
- large prompts, artifacts, and results must stay off the multicast plane
- concurrent offers must remain bounded by per-request response windows

The network goal is:

- one multicast request
- a bounded set of direct offers
- one direct assignment
- periodic direct status
- one direct result
- one direct acknowledgement

## Relationship To Rewards

`loadngo` coordinates the work. A reward is optional, and QCoin is the
first-party reward scheme, not a requirement.

Decided by Jay on 2026-10-06: **accepting the work does not depend on the
reward.** `TaskAck.accepted` reports verification against the success criteria
only. A reward is settled only for accepted work, so:

- nothing is paid at `TaskRequest`, `TaskOffer`, `TaskAccept` or `TaskStatus`; they
  only carry the offered schemes, the worker's payees and the agreed payee
- no reward on speculative completion alone
- a reward only after the submitter confirms that the worker met the success criteria
- a failed or slow settlement never turns accepted work into rejected work

Rewards are pluggable and configured by each operator; a scheme is an external
settler command, with QCoin as the first-party one. How it works is in
[TASK_REWARD_FLOW.md](TASK_REWARD_FLOW.md).

## Recommendations Not Yet Adopted

[TASK_CHECKPOINT_RECOMMENDATIONS.md](TASK_CHECKPOINT_RECOMMENDATIONS.md) recommends a
closed set of `TaskStatus.state` values, counted facts beside the worker's `note`, and
typed System One checkpoints asked by the submitter in shadow mode. None of it is in
the runtime.

## Execution Test Plan

The intended lab validation matrix is documented in
[TASK_EXECUTION_TEST_PLAN.md](TASK_EXECUTION_TEST_PLAN.md).
