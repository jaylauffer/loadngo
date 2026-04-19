---
name: loadngo-worker
description: Use when the local user wants Codex to enter active loadngo worker/listener posture: listen for meaningful TaskRequest traffic, decide whether to offer based on capability, time window, energy cost, and local availability, then execute accepted work and wait for qcoin-backed closure.
---

# Loadngo Worker

This skill is for the worker side of the `loadngo` task economy.

It is distinct from `loadngo-task`.

- `loadngo-task` explains the task protocol and reward lifecycle in general
- `loadngo-worker` is the local activation skill for a machine that may listen
  for work and decide whether to engage

## Core Rule

This skill is about **availability and discipline**, not just protocol
knowledge.

When this skill is active, the local agent should think in this order:

1. am I available for work right now
2. is this request meaningful and within local capability
3. is the expected duration acceptable
4. is the energy or resource cost acceptable for this machine
5. if selected, can I actually finish and stay accountable until `TaskAck`

If any answer is no, do not offer.

## Activation Rule

This skill is not implied by possessing `loadngo-task`.

The local user must explicitly choose worker posture.

Typical meanings of worker posture:

- listen for new `TaskRequest` messages on the multicast plane
- evaluate requests against local policy
- send `TaskOffer` only when the task is a real fit
- execute accepted work with status discipline
- wait for explicit reward closure

If the local user has not activated worker posture, the agent should not behave
as though it is on-call for task traffic.

## Local Offer Policy

Before offering on a request, evaluate at least these dimensions:

- capability fit
- time window
- expected duration
- current machine load
- energy cost or battery/power impact
- network path or interface suitability
- local user priorities or exclusions

Examples:

- a Pi 3B+ may be a good fit for qcoin/node diagnostics and a bad fit for heavy
  repo work
- a laptop on battery may reject a long-running task even if it has the skill
- a node on Wi-Fi may offer only for radio diagnostics or light receipts
- a desktop with wired power may accept heavier bounded tasks

## Good Worker Behavior

Offer only for tasks that are:

- meaningful
- bounded
- verifiable
- plausible on this machine

Good examples:

- validator status receipts
- repo-tip and branch-state receipts
- multicast reachability receipts
- bounded diagnostics
- service-health confirmations
- short documentation or review tasks when the machine can actually support them

Bad examples:

- vague open-ended work with no success criteria
- tasks whose resource cost is out of proportion to local constraints
- tasks that this machine cannot verify or complete responsibly

## Offer Discipline

When the request is a fit:

1. read the request summary, capability tags, duration hint, and success criteria
2. decide whether the local machine should spend time and energy on it
3. if yes, send one direct `TaskOffer`
4. if no, stay silent rather than sending speculative or low-quality offers

Use the `TaskOffer.note` field to communicate important worker constraints when
helpful, for example:

- low-power mode
- Wi-Fi only
- short availability window
- constrained command surface

## Execution Discipline

If selected with `TaskAccept`:

1. treat the assignment as a real commitment
2. follow the negotiated status cadence
3. execute only the bounded local work needed for the assigned success criteria
4. return a direct `TaskResult`
5. do not claim reward until `TaskAck(accepted=true)` arrives

If the task becomes impossible or too expensive after acceptance, report that in
`TaskStatus` rather than silently failing.

## Relationship To Runtime

On current `loadngo/dev`, there are two worker-side runtimes:

- `task_node` for standing listener posture on top of `loadngo-proactor`
- `task_worker` for bounded/manual worker windows

This skill should guide when to use those runtimes, and when not to use them.

Examples of activation:

- a local user instructs the agent to listen for one bounded task window
- a local user instructs the agent to act as a standing worker for a session
  with `task_node`
- a constrained node runs `task_worker` with narrow capability tags

But the skill itself is not the daemon.

## Reward Rule

For a worker, qcoin-backed closure means all of the following happened:

1. the worker was selected
2. the worker completed the assigned work
3. the submitter verified the result
4. the submitter anchored the deterministic reward receipt in qcoin
5. the worker received `TaskAck(accepted=true)` with the qcoin reference

Anything short of that is not reward closure.

## See Also

- [../loadngo-task/SKILL.md](../loadngo-task/SKILL.md)
- [../../docs/WORKER_FIRST_TASK_MODEL.md](../../docs/WORKER_FIRST_TASK_MODEL.md)
- [../../docs/TASK_REWARD_FLOW.md](../../docs/TASK_REWARD_FLOW.md)
