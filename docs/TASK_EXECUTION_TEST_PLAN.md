# Task Execution Test Plan

Purpose: define the lab validation plan for the `loadngo` submitter/worker
protocol over IPv6 multicast discovery and direct unicast follow-up.

## Scope

This plan validates:

- request discovery over IPv6 multicast
- direct unicast offers back to the submitter
- correlation under concurrent offers
- explicit worker selection by the submitter
- negotiated status cadence and delivery thresholds
- reassignment or self-execution after stale work
- reward gating so qcoin is awarded only after acknowledged completion

It does not yet cover transport security, signature verification, or artifact
payload transfer.

## Lab Shape

Suggested nodes:

- MacMini submitter on wired and radio IPv6
- `agnes` worker on wired and radio IPv6
- `dolores` worker on wired and radio IPv6
- `gretta` worker on radio IPv6, potentially as a constrained non-Codex worker

Suggested multicast group:

- the `loadngo` IPv6 task multicast group currently used by the lab

All nodes should:

- bind dual-stack or at least IPv6 UDP
- join the IPv6 multicast group on all intended lab interfaces
- expose one direct unicast reply endpoint for task follow-up

## Test Cases

### 1. Single Request, Single Offer, Successful Completion

1. submitter multicasts one `TaskRequest`
2. one worker responds with one correlated `TaskOffer`
3. submitter selects that worker with `TaskAccept`
4. worker emits at least one `TaskStatus`
5. worker emits `TaskResult`
6. submitter validates success criteria and sends `TaskAck(accepted=true)`
7. qcoin reward is minted or anchored after the positive acknowledgement

Pass criteria:

- the IDs stay stable across every message
- no other worker assumes ownership
- qcoin reward happens only after the positive acknowledgement

### 2. Single Request, Multiple Concurrent Offers

1. submitter multicasts one `TaskRequest`
2. at least two workers respond with direct `TaskOffer` messages for the same `request_id`
3. submitter inspects both offers
4. submitter selects only one worker with `TaskAccept`

Pass criteria:

- every offer carries the same `request_id`
- each worker uses its own `offer_id`
- the submitter can distinguish and choose among offers without collision
- only the selected worker proceeds into status/result traffic

### 3. Unrelated Concurrent Requests

1. two submitters multicast two different `TaskRequest` messages
2. workers respond to whichever requests they can satisfy

Pass criteria:

- workers do not confuse one request with another
- offers remain correlated to the correct `request_id`
- the submitter does not mix offers from unrelated requests

### 4. Negotiated Status Cadence

1. submitter selects a worker with `TaskAccept`
2. `TaskAccept` carries `status_check_interval_secs`
3. worker sends `TaskStatus` within that interval

Pass criteria:

- the worker's status cadence stays within the assigned interval
- the submitter can compute the next expected check-in from the accepted terms

### 5. Missed Status Check

1. submitter selects a worker
2. worker misses the negotiated status interval

Pass criteria:

- submitter marks the assignment stale
- submitter can either reissue the request or self-perform the work
- no qcoin reward is emitted for the stale assignment

### 6. Delivery Threshold Exceeded

1. submitter selects a worker
2. worker sends some status but misses the agreed delivery threshold

Pass criteria:

- submitter detects threshold breach
- submitter can reissue the request or self-perform
- old assignment remains unrewarded unless later explicitly accepted

### 7. Result Rejected

1. worker submits `TaskResult`
2. submitter checks the result against success criteria
3. submitter responds with `TaskAck(accepted=false)`

Pass criteria:

- the rejection remains correlated to the same assignment
- no qcoin reward is emitted
- the submitter can reopen or reissue the work

### 8. Result Accepted And Rewarded

1. worker submits `TaskResult`
2. submitter verifies the success criteria
3. submitter responds with `TaskAck(accepted=true)`
4. qcoin reward is minted or anchored and referenced back to the work record

Pass criteria:

- reward happens only after positive acknowledgement
- the reward can be tied back to the completed assignment
- the worker receives credit only once for the accepted assignment

## Observability

For each test, capture:

- the node IDs involved
- the interface and address family used
- `request_id`
- `offer_id`
- `assignment_id`
- timestamps for request, offer, accept, status, result, and ack
- qcoin reward reference when applicable

Useful outputs include:

- CLI logs from `task_request` and `task_offer`
- direct UDP packet captures where necessary
- qcoin transaction or anchor references after positive acknowledgement

## Immediate Minimum Run

The minimum meaningful field run is:

1. MacMini submits one `TaskRequest` over IPv6 multicast
2. `agnes`, `dolores`, and `gretta` run worker-side `task_offer`
3. verify at least two concurrent offers arrive directly at the MacMini
4. manually select one worker with a direct `TaskAccept`
5. run a real worker-side task command that produces a durable artifact
6. verify the artifact from the submitter side
7. record `TaskAck(accepted=true)` with the included qcoin transaction reference

That run is enough to prove that correlation, concurrency, timeout policy, and
reward gating are coherent before automating the full lifecycle.

For `gretta`, the assigned task should respect a Pi 3B+ capability ceiling and
still be meaningful, for example a qcoin validator receipt, radio/multicast
reachability receipt, or bounded repo/service diagnostic.
