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
7. qcoin reward happens only after a positive acknowledgement

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
8. only after positive `TaskAck` should qcoin reward be minted or anchored

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
- `accepted`
- optional `qcoin_tx_hint`
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

## Relationship To qcoin

`loadngo` coordinates the work.

`qcoin` rewards acknowledged completion.

That means:

- no qcoin award on `TaskRequest`
- no qcoin award on `TaskOffer`
- no qcoin award on `TaskAccept`
- no qcoin award on `TaskStatus`
- no qcoin award on speculative completion alone
- qcoin award only after the submitter confirms that the worker met the success criteria

`TaskAck(accepted=true)` is the reward gate.

For the current runtime, the submitter may withhold the positive wire-level
`TaskAck` until the qcoin reward anchor is durably included, so the worker gets
one closure message that carries the qcoin reference.

The actual qcoin mint or anchor may happen immediately after that acknowledgement
or through a downstream authority path, but it must remain downstream of the
positive acknowledgement.

## Execution Test Plan

The intended lab validation matrix is documented in
[TASK_EXECUTION_TEST_PLAN.md](TASK_EXECUTION_TEST_PLAN.md).
