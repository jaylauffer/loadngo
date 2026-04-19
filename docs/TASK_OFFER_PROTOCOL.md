# Task Offer Protocol

Purpose: define the first-pass `loadngo` task coordination contract for worker
nodes operating over multicast discovery with unicast follow-up.

This note is intentionally narrow. It covers only:

- offer advertisement
- worker response
- anti-amplification rules
- immediate execution posture for worker nodes

It does not define the full authority, acknowledgement, or qcoin reward
contract. Those remain follow-on layers above this transport shape.

## Core Rule

`TaskOffer` is multicast advertisement.

It is not authoritative assignment.

That means:

- an offerer announces available work to reachable peers
- worker nodes may respond directly to the offerer
- only the offerer chooses which worker, if any, should execute the task

## Traffic Shape

The first-pass flow is:

1. `TaskOffer` goes to multicast discovery targets
2. `TaskAccept` goes back to the offerer via unicast
3. `TaskConfirm` goes from the offerer to one chosen worker via unicast
4. `WorkClaim` goes from the worker to the offerer or authority via unicast
5. `AckResponse` goes back via unicast

Multicast is for discovery and lightweight offer visibility only.

All follow-up traffic is direct.

## Anti-Amplification Rule

`TaskOffer` must drain, not amplify.

Required rules:

- workers must never rebroadcast a received `TaskOffer`
- workers must reply only to the offerer, never to multicast
- workers must emit at most one `TaskAccept` per `offer_id`
- offerers must deduplicate repeated accepts from the same worker
- offerers must not acknowledge accepts by multicast
- large payloads, artifacts, and result data must never be sent by multicast

The intended behavior is:

- one multicast offer
- a bounded set of direct unicast responses
- one direct assignment decision

That keeps a busy subnet from turning one work announcement into packet fanout.

## Offer Semantics

A `TaskOffer` should carry only the minimum data needed for a worker to decide
whether to respond:

- `offer_id`
- `task_id`
- `offerer_node_id`
- `created_at`
- `expires_at`
- `summary`
- `capability_tags`
- `reply_endpoints`
- optional `artifact_hint`

It should not carry:

- full artifacts
- large embedded prompts
- result payloads
- qcoin reward material

If a worker needs more context, it should request it over direct follow-up or
receive it in the later unicast assignment path.

## Worker Semantics

A worker node should treat a multicast offer as visibility, not ownership.

Worker behavior:

- inspect the offer once
- decide whether it can perform the work
- send one unicast `TaskAccept` if it is willing
- wait for explicit confirmation before assuming ownership

If no confirmation arrives before the offer expiry, the worker should discard
the pending interest state.

## Offerer Semantics

The offerer is responsible for race closure.

Offerer behavior:

- announce the offer by multicast
- collect direct accepts during a bounded response window
- choose one worker or self-execute
- send direct confirmation to the selected worker
- optionally send direct decline notices to others

If no acceptable worker responds, the offerer may become the worker of record
and execute locally.

## Discovery And Transport

Preferred order:

1. IPv6 multicast discovery
2. IPv4 multicast discovery when available
3. direct unicast follow-up over the most recently working path

The transport rule remains:

- multicast for advertisement only
- unicast for negotiation, progress, claims, and acknowledgements

## Relationship To qcoin

`TaskOffer` does not mint qcoin.

At most, completed and acknowledged work may later become eligible for a qcoin
reward or anchor flow.

That reward decision belongs to a later authority layer.

So the transport stack should remain cleanly separated:

- `loadngo` discovers peers and carries task traffic
- authority decides whether work is complete and acknowledged
- qcoin only receives selected post-acknowledgement reward or proof material

## Immediate First Pass

The first useful implementation slice is:

1. encode `TaskOffer` and `TaskAccept`
2. send offers by multicast
3. receive accepts by unicast
4. self-execute on timeout when no peer is chosen

That is enough to exercise real worker-node behavior without pretending the
full authority and reward layers already exist.
