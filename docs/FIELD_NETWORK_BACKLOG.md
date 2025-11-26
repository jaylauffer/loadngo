# Field Network Backlog

## Purpose

This document turns the current `loadngo` architecture into an implementation
backlog for an offline-first field network that can operate under weak,
intermittent, or absent connectivity.

The intended use is not "replace all cloud systems." The intended use is:

- keep a local authority node working when the internet is absent
- preserve important state and chain-of-custody
- reconcile safely later
- support physical relay when radio or IP paths fail

This is the field-operations counterpart to:

- [ARCHITECTURE.md](ARCHITECTURE.md)
- [PROACTOR_ARCHITECTURE.md](PROACTOR_ARCHITECTURE.md)
- [PUDDING_CAS_PQ_MODEL.md](PUDDING_CAS_PQ_MODEL.md)

## Current Foundation

The current codebase already provides useful substrate pieces.

### Platform and runtime foundation

- platform-agnostic host boundaries and UI contracts exist in
  [host-core/src/lib.rs](../host-core/src/lib.rs)
- the intended event-driven runtime model exists in
  [proactor/src/lib.rs](../proactor/src/lib.rs)

### Content-addressed storage foundation

- CAS hashing and storage exist in
  [data/src/cas.rs](../data/src/cas.rs)
- incomplete writes, later verification, and content listing already exist
- workspace ingestion and manifest creation already exist in
  [data/src/bin/pudding_cas_ingest.rs](../data/src/bin/pudding_cas_ingest.rs)

### Transport foundation

- UDP transport, multicast bootstrap, frame send/receive, and timeouts exist in
  [network/src/lib.rs](../network/src/lib.rs)
- chunked content transfer into CAS exists in
  [network/src/core.rs](../network/src/core.rs)
- higher-level `SneakerNet` request/response content transfer exists in
  [network/src/p2p.rs](../network/src/p2p.rs)

### Verified current behavior

Validated locally:

- `cargo test -p data -p network` passes
- content can be requested by hash and reconstructed into receiver CAS
- missing chunks and corrupt payloads are detected
- incomplete CAS entries can remain open for later completion

Relevant tests:

- [network/tests/sneakernet.rs](../network/tests/sneakernet.rs)
- [network/tests/p2p_protocol.rs](../network/tests/p2p_protocol.rs)
- [network/tests/core.rs](../network/tests/core.rs)
- [data/tests/cas_storage.rs](../data/tests/cas_storage.rs)

## What Is Missing

The current implementation is not yet a deployable life-critical field system.

Major gaps:

- no incident-oriented manifest schema
- no operator/device identity and trust model on the wire
- no transport encryption or message authentication
- no finished retransmit/resume logic for missed UDP chunks
- no replica promotion or signed lineage verification
- no domain workflows for intake, triage, inventory, dispatch, or delivery proof
- no field-grade operator UX, alerting, or stale-data semantics
- proactor integration is incomplete on active platform paths

Important current limitations:

- `TransferFileMissed` exists in the protocol but is not handled yet in
  [network/src/p2p.rs](../network/src/p2p.rs)
- participant registration is still a placeholder in
  [network/src/lib.rs](../network/src/lib.rs)
- PQ lineage is a design target, not a finished implementation, per
  [PUDDING_CAS_PQ_MODEL.md](PUDDING_CAS_PQ_MODEL.md)
- the Windows `IOCP` backend currently does not satisfy the `Send + Sync`
  requirement of `CompletionPort`, so `loadngo-proactor` does not currently
  build on Windows

## Deployment Model

The recommended deployment model is "portable local authority with delayed
reconciliation."

### Node roles

1. Local authority node
- clinic, warehouse, vehicle, or field-command laptop
- owns the writable local truth for its operating cell
- stores authoritative manifests and CAS blobs for that cell

2. Operator device
- tablet, phone, or workstation used by responders
- syncs against a local authority node over LAN, hotspot, or direct link
- may hold a materialized replica for offline read/write capture

3. Relay node
- another laptop, vehicle node, or removable media kit
- carries signed manifests and missing blobs between disconnected cells

4. Regional consolidation node
- validates lineage
- imports replicas
- reconciles cross-cell state
- does not erase local accountability history

### Exchange order

1. exchange signed manifests first
2. compare lineage and missing blob hashes
3. request only missing content by hash
4. promote verified content into local CAS
5. record replica observation and acknowledgement

This sequence minimizes bandwidth and supports physical relay.

## Recommended Build Order

## Phase 0: Safety Boundaries And Operating Assumptions

Define and freeze:

- what local nodes are allowed to authoritatively decide
- what can be edited offline
- what requires later confirmation
- stale-data rules
- conflict classes: mergeable, review-required, forbidden

Deliverables:

- field authority model document
- risk classification for every record type
- operator-visible stale/conflict states

Exit criteria:

- every future schema and protocol change can point to an authority rule

## Phase 1: Incident Bundle Manifest Schema

Build a domain manifest format distinct from the current workspace/git manifest.

Add under `data`:

- manifest type for field records
- manifest id and versioning
- record envelope with timestamps, origin node, actor, and schema version
- parent/ancestor references
- attachment/blob references by CAS hash
- materialization status and replica observations

Suggested first record families:

- incident
- site
- person or patient pseudonymous record
- inventory item
- asset or vehicle
- dispatch task
- delivery or handoff confirmation

Likely crates:

- [data/src/cas.rs](../data/src/cas.rs)
- new manifest module under `data/src`

Exit criteria:

- a single signed manifest can describe a coherent field-state snapshot
- all large attachments are externalized as CAS blobs

## Phase 2: Bundle Export And Import

Turn the current CAS ingest direction into a general bundle workflow.

Build:

- export command for manifest plus referenced blobs
- import command that verifies manifest, imports missing blobs, and records the
  replica observation
- resumable copy layout for USB, SD card, or other removable media

Suggested bundle shape:

- `manifest.json` or `manifest.ron`
- `signatures/`
- `objects/xx/<hash>.bin`
- `receipts/`

Likely crates:

- `data`
- `network` for shared serialization helpers

Exit criteria:

- two disconnected nodes can exchange state entirely by removable media
- import is idempotent
- imported blobs are verified before promotion

## Phase 3: Signed Lineage And Trust Roots

Move from plain integrity to authenticated lineage.

Build:

- signed manifest envelope
- trust root store
- signer identity model for nodes and operators
- manifest verification result types
- lineage validation rules

Initial scope:

- classical signatures are acceptable first if they unblock real validation
- algorithm agility must remain in the type system
- PQ support should remain the target, not a blocker for the first end-to-end
  field deployment

Likely crates:

- `data`
- new crypto/signature module

Current note:

- `loadngo-pq-auth` now exists as an initial signed challenge-token foundation
  for operator/node authentication semantics
- it is not yet a full trust-root store or manifest-acceptance policy layer

Exit criteria:

- imported manifests can be accepted, quarantined, or rejected with explicit
  reasons
- authority and ancestry can be verified without trusting transport

## Phase 4: Transport Hardening

Finish the network behavior required for unreliable links.

Build:

- manifest-first sync messages
- missing-chunk bitset exchange
- actual `TransferFileMissed` handling
- retransmit windowing
- duplicate suppression
- resume after interrupted transfer
- message size and pacing rules

The current UDP transport is a reasonable starting point, but it is still too
thin for field use.

Likely crates:

- [network/src/lib.rs](../network/src/lib.rs)
- [network/src/core.rs](../network/src/core.rs)
- [network/src/p2p.rs](../network/src/p2p.rs)

Exit criteria:

- packet loss does not require full restart of a content transfer
- nodes can resume a partially received bundle
- receiver can explicitly request missing ranges or chunks

## Phase 5: Replica And Reconciliation Model

Turn "copied somewhere else" into explicit state.

Build:

- replica records
- acknowledgement receipts
- promotion states: materialized, verified, authoritative, archived
- conflict records
- reconciliation journal

This should follow the distinctions already described in
[PUDDING_CAS_PQ_MODEL.md](PUDDING_CAS_PQ_MODEL.md).

Exit criteria:

- an operator can tell whether data is local-only, relayed, verified remotely,
  or merged into a broader lineage

## Phase 6: Runtime Integration And Platform Ports

Move the network/storage path onto the intended runtime model.

Build:

- integrate network and sync work with `loadngo-proactor`
- remove dependence on blocking polling loops for active sync paths
- finish Windows IOCP integration
- add Linux `epoll` backend
- add Android `ALooper` backend if mobile field nodes are required

Immediate Windows fix:

- make `IocpPort` satisfy the `CompletionPort: Send + Sync` contract safely
- restore passing `loadngo-proactor` tests on Windows

Likely crates:

- [proactor/src/lib.rs](../proactor/src/lib.rs)
- [proactor/src/iocp.rs](../proactor/src/iocp.rs)
- `network`

Exit criteria:

- sync, timers, and wakeups are event-driven on supported platforms
- `cargo test -p loadngo-proactor` passes on Windows

## Phase 7: Field Workflow Surfaces

After storage and transport are trustworthy, build the domain UX.

Suggested first operator surfaces:

- intake queue
- inventory and stock movement
- dispatch board
- handoff confirmation
- stale/conflict review
- sync status and relay health

UX requirements:

- large-state indicators
- explicit sync freshness
- explicit authority source
- explicit conflict and quarantine handling
- low-cognitive-load interaction for stressed operators

Likely crates:

- `task`
- `gui`
- `ui-core`

Exit criteria:

- a responder can complete a core workflow without interpreting raw manifests,
  hashes, or transport state

## Phase 8: Verification And Failure Drills

Build the validation harness before claiming operational readiness.

Required test classes:

- packet loss and reordering
- interrupted transfer resume
- stale manifest replay
- wrong-signer rejection
- media corruption on physical relay
- clock skew
- duplicate import
- split-brain local edits across disconnected cells

Required non-test exercises:

- tabletop incident drill
- battery/power-loss recovery drill
- removable-media relay drill
- operator handoff drill

Exit criteria:

- failure behavior is understandable and recoverable by operators

## Minimal Production Slice

If the goal is an early but credible field deployment, the minimum useful slice
is:

1. incident manifest schema
2. signed manifest verification
3. export/import by removable media
4. hash-addressed missing-content fetch
5. replica state and acknowledgement
6. basic stale/conflict operator view

That slice is more valuable than prematurely building live multi-hop mesh
routing.

## What To Avoid Early

Do not lead with:

- generalized peer-to-peer consensus
- full automatic conflict merging for all record types
- heavy live-mesh routing protocols
- runtime-generated cryptographic policy complexity
- broad cloud dependency

Those are expensive and will slow down the local-authority model that already
fits the architecture.

## Immediate Next Tasks

The next concrete implementation tasks should be:

1. create a `field_manifest` module in `data`
2. define signed manifest envelope and verification result types
3. implement bundle export/import around CAS and manifest references
4. implement real missing-chunk recovery in `network`
5. fix Windows `IOCP` conformance in `loadngo-proactor`

## Summary

`loadngo` already has the right backbone for resilient field operation:

- platform-agnostic host seams
- local-first storage
- hash-addressed content transfer
- a deliberate lineage and replication direction

To become something that can credibly support life-preserving operations, it
now needs:

- authenticated lineage
- bundle-based relay
- reliable transfer recovery
- explicit replica state
- domain workflows for responders
