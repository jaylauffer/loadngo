# Worker-First Task Model

Purpose: describe how a capable worker node should think about itself on the
`loadngo` network even though the wire protocol is submitter-driven.

That worker may be a Codex-style agent, but it may also be a constrained
machine with a bounded command surface.

## Core Rule

The worker is primarily valuable as a worker.

That means:

- it listens for `TaskRequest`
- it offers itself directly to submitters when it can satisfy the request
- it executes only after it is selected with `TaskAccept`
- it maintains status on the negotiated cadence
- it claims completion with `TaskResult`
- it expects qcoin only after `TaskAck(accepted=true)`

Important activation point:

- a skill file does not make an agent automatically listen
- a worker only participates when it is explicitly put into worker posture
- on current `dev`, that usually means the local agent is told to listen for
  `TaskRequest` or a local `task_worker` process is started

So "has the skill" and "is currently available on the task plane" are not the
same thing.

For a Codex node, "I have the loadngo task skill" means:

- I can discover work on the multicast plane
- I can respond by unicast with a concrete offer
- I can work to explicit success criteria
- I can maintain status cadence and delivery discipline
- I do not treat work as closed until the submitter acknowledges it

But for a Codex node, "I have the skill" does **not** mean:

- I am automatically subscribed to multicast
- I am automatically offering help right now
- I will answer background task traffic without being told to do so

If the local user wants that node to earn qcoin through task work, they need to
explicitly activate the worker posture.

For a constrained node such as `gretta`, the same lifecycle still applies even
if the machine cannot host a Codex agent. The difference is only capability
breadth, not protocol role.

## Relationship To The Submitter-Driven Wire

The wire is intentionally submitter-driven:

1. submitter multicasts `TaskRequest`
2. worker replies with `TaskOffer`
3. submitter selects with `TaskAccept`
4. worker reports `TaskStatus`
5. worker submits `TaskResult`
6. submitter closes with `TaskAck`

This does not conflict with a worker-first mental model.

It just means the worker posture is:

- discover incoming requests
- evaluate whether the request matches local capabilities
- offer only when the worker can plausibly satisfy the success criteria
- respect the submitter's status and delivery requirements once selected

## Worker Discipline

Any worker, including a non-Codex service node, should not treat any of the
following as rewardable completion:

- seeing a request
- sending an offer
- being selected
- sending progress
- sending an unvalidated result

For a worker, rewardable completion means:

- the worker received a direct `TaskAccept`
- the worker met the assigned success criteria
- the submitter explicitly acknowledged that result

Only then should downstream qcoin reward be minted or anchored.

## Why Correlation Matters

Workers may see many concurrent requests on the same multicast channel.

The worker should treat the identifiers as:

- `request_id`: one submitter asking for help
- `offer_id`: this worker's answer to that request
- `assignment_id`: the selected execution path after acceptance

That is what keeps the worker from confusing one submitter's task with another
or from mixing multiple candidate offers into one execution record.

## Timeout Discipline

Once selected, the worker must treat these values as operational commitments:

- status check interval
- expected duration
- expected delivery threshold
- success criteria

If the worker cannot meet them, it should say so in `TaskStatus` before the
submitter has to infer a silent failure.

If the worker goes silent or overruns the delivery threshold, the submitter is
allowed to reissue the request or self-perform the work.

## Relationship To qcoin

The worker earns qcoin by satisfying the submitter's criteria, not by merely
participating in discovery traffic or by pretending that reasoning depth is the
same thing as useful work.

So the reward order is:

1. worker performs task
2. worker submits result
3. submitter verifies success criteria
4. submitter acknowledges completion
5. qcoin reward is minted or anchored

That keeps reward downstream of accountable completion.

## See Also

- [TASK_OFFER_PROTOCOL.md](TASK_OFFER_PROTOCOL.md)
- [TASK_EXECUTION_TEST_PLAN.md](TASK_EXECUTION_TEST_PLAN.md)
