---
name: loadngo-task
description: Use when the user wants Codex to request, assign, verify, or document meaningful loadngo lab tasks over the multicast/unicast task plane, or when Codex needs to close acknowledged task work with qcoin-backed reward anchoring. Also use when updating the loadngo task workflow, task binaries, or task-skill documentation.
---

# Loadngo Task

This skill is for the real `loadngo` lab workflow, not placeholder queue demos.

Workers on this substrate do not need to be Codex agents.

A worker may be:

- a Codex agent host
- a constrained scripted node
- a validator or service node that can run a bounded command set

So a Raspberry Pi such as `gretta` can still earn qcoin-backed credit if it can
accept meaningful work, return a verifiable artifact, and satisfy the
submitter's success criteria.

The current task model is submitter-driven:

1. `TaskRequest` multicast discovery
2. `TaskOffer` direct worker response
3. `TaskAccept` direct worker selection
4. `TaskStatus` direct progress
5. `TaskResult` direct completion claim
6. `TaskAck` direct closure
7. qcoin reward anchor only after positive acknowledgement

## Activation Rule

This skill is not an always-on daemon.

Installing or acquiring the skill does **not** by itself make a Codex agent
listen on the `loadngo` task multicast plane.

For a remote agent or worker host to participate in the task economy, one of
these must also be true:

- the local user explicitly instructs that agent to enter worker posture and
  respond to `TaskRequest`
- a local worker process such as `task_worker` is running on that machine
- a future always-on task bridge exists on that machine

So "the agent has the skill" and "the agent is currently listening for work"
are different states.

If the user's intent is "earn qcoin by helping on the lab task plane," the
agent should be told to do both:

1. use the `loadngo-task` skill
2. enter worker/listener posture

Without step 2, no `TaskOffer` should be expected.

## Use This Skill When

- the user wants Codex to act as a `loadngo` submitter, or to coordinate one or more `loadngo` workers
- the user wants real lab task sequences run across the MacMini, `agnes`, `dolores`, or `gretta`
- the user wants `loadngo` task work turned into qcoin-backed proof
- the user wants the `loadngo` task protocol or task binaries updated
- the user wants task work documented as a Codex skill or reusable workflow

Do not use this skill for generic issue tracking, abstract project planning, or
dummy "echo hello" workloads.

## Real-State Checks

Before acting on live lab state:

1. Refresh `loadngo` and the sibling `qcoin` repo with `git fetch --all`.
2. Read the current protocol docs only as needed:
   - `docs/TASK_OFFER_PROTOCOL.md`
   - `docs/TASK_EXECUTION_TEST_PLAN.md`
   - `docs/WORKER_FIRST_TASK_MODEL.md`
   - `../qcoin/docs/LAB_CLUSTER_WORKING_PROOF.md`
3. Verify the live qcoin cluster over the UDP wire, not the old HTTP adapter:
   - `cargo run -q -p qcoin-node --manifest-path ../qcoin/Cargo.toml -- node-info --target <host:9700>`
   - `cargo run -q -p qcoin-node --manifest-path ../qcoin/Cargo.toml -- tip --target <host:9700>`

If the live repo state or cluster state disagrees with memory, trust the live
state and update the plan.

## Meaningful Work Rule

Only request or accept work that produces a durable lab-useful artifact or state
change, for example:

- node diagnostics and interface receipts
- repo-tip and service-health reports
- validated protocol/test receipts
- durable documentation updates
- repair or rollout steps with observable completion evidence

Avoid synthetic tasks whose only proof is that a command returned zero.

## Task Lifecycle Rule

Use these identifiers consistently:

- `request_id`: one submitter request
- `offer_id`: one worker response
- `assignment_id`: the selected execution path

Treat completion as real only when all of the following are true:

- the worker was selected with `TaskAccept`
- the worker returned the requested artifact or result
- the submitter checked the success criteria
- the submitter sent `TaskAck(accepted=true)`

Do not describe a worker as rewarded before the acknowledgement step.

## qcoin Reward Rule

In the current lab, "rewarded qcoin" means a deterministic qcoin-backed reward
anchor or proof record unless the code explicitly proves native QCOIN issuance.

Keep these states separate:

- work completed
- qcoin transaction accepted
- qcoin transaction included

Only claim qcoin-backed reward closure after durable inclusion is observed.

## Preferred Working Pattern

1. Pick a meaningful task that fits the worker's actual machine role and capability ceiling.
2. Run the request/offer/accept path on the `loadngo` task plane.
3. Retrieve or inspect the resulting artifact directly.
4. Check the artifact against the assigned success criteria.
5. Send `TaskAck`.
6. Submit the deterministic qcoin reward anchor.
7. Confirm inclusion on the converged qcoin cluster.
8. Record the receipt with:
   - `request_id`
   - `offer_id`
   - `assignment_id`
   - worker node id
   - artifact path or hash
   - qcoin transaction id
   - inclusion evidence

For constrained workers such as `gretta`, prefer tasks that produce crisp
machine-verifiable outputs, for example:

- repo-tip and branch-state receipts
- qcoin node-info or tip receipts
- service health and interface diagnostics
- packet capture or multicast reachability receipts
- bounded test or rollout confirmations

For Codex-capable worker hosts such as `agnes` or `dolores`, the local operator
should also decide whether the host is:

- temporarily available for one task
- explicitly listening for new `TaskRequest`
- or idle and not participating in the task economy yet

## Remote Hosts

Known lab SSH targets:

- `jay@10.10.10.1` for `agnes`
- `jay@10.10.10.6` for `dolores`
- `jay@192.168.1.140` for `gretta`

Before changing repo state on those machines, inspect their branch and worktree
first so you do not overwrite ongoing local agent work.

## Failure Rules

- If `TaskOffer` exists but `TaskAccept` or later-stage tooling is missing, say so and implement the missing minimum before claiming a full cycle.
- If qcoin only reports acceptance or mempool presence, do not collapse that into inclusion.
- If the task is meaningful but the current tooling cannot verify it, stop short of reward closure and say exactly what evidence is missing.

## Fast Reference

- `loadngo` repo root: current repository root
- sibling `qcoin` repo: `../qcoin`
- protocol note: `docs/TASK_OFFER_PROTOCOL.md`
- test plan: `docs/TASK_EXECUTION_TEST_PLAN.md`
- qcoin proof note: `../qcoin/docs/LAB_CLUSTER_WORKING_PROOF.md`
