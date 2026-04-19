# Worker-First Task Model

Purpose: define the intended `loadngo` coordination stance for Codex-style
agents operating as workers on the network plane.

This note supersedes any assumption that the local agent is primarily the
offerer or dispatcher.

## Core Rule

The agent is the worker.

It uses `loadngo` to discover or request work, executes that work to explicit
success criteria, and only then becomes eligible for `qcoin` reward minting or
reward anchoring.

So the intended control flow is:

1. worker asks for work or advertises availability
2. authority or broker responds with concrete work
3. worker executes to stated success criteria
4. authority validates completion
5. reward logic runs only after validation

## Transport Shape

Preferred flow:

1. `TaskRequest` or `WorkerHello` goes out over multicast discovery
2. `TaskOffer` or `TaskAssignment` comes back by unicast
3. `TaskAccept` goes back by unicast
4. progress, artifact references, and `WorkClaim` stay on unicast
5. `AckResponse` closes the work record
6. `qcoin` reward or proof material is produced only after acknowledgement

Multicast should carry only lightweight discovery and availability signals.

Unicast should carry:

- assignment
- success criteria
- artifact references
- progress
- completion claim
- acknowledgement

## Success Criteria

A worker should not treat "task performed" as sufficient.

A task is complete only when:

- the assignment included explicit success criteria
- the worker returned the required artifact references or proof material
- the authority acknowledged that the criteria were satisfied

That acknowledgement is the gate for downstream reward handling.

## Relationship To qcoin

`qcoin` should not mint on:

- worker discovery
- availability advertisement
- task request
- task acceptance
- speculative completion

`qcoin` should only mint or anchor reward material after:

- work completion was claimed
- the claim was checked against the assigned success criteria
- an acknowledgement closed the task

That keeps the reward plane downstream of the coordination plane.

## Relationship To The Existing TaskOffer Note

[TASK_OFFER_PROTOCOL.md](TASK_OFFER_PROTOCOL.md) is still useful as a bootstrap
description for multicast advertisement with unicast follow-up.

But it is sender-centric.

It is not the intended steady-state mental model for Codex workers.

The intended mental model is worker-first:

- workers seek or request work
- authorities assign work
- rewards follow acknowledgement

## Immediate Implementation Order

The correct implementation order is:

1. add a worker-originated discovery/request message
2. add a long-running worker process that advertises capabilities and reply
   endpoints on the local network plane
3. let authorities respond by unicast with concrete assignments
4. define acknowledgement and reward-closing messages
5. bridge acknowledged work into `qcoin`

## Practical Working Assumption

For agent behavior in this repository:

- "I have the loadngo task skill" means:
- I am a worker on the `loadngo` substrate
- I should seek or request work through that substrate
- I should complete work to explicit success criteria
- I should expect reward logic only after acknowledgement
