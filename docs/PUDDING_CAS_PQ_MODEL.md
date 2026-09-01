# Pudding CAS PQ Model

Purpose: define the intended post-quantum repository model for the
`/Users/jay/pudding` workspace when using `loadngo` content-addressed storage as
the primary accountability layer.

Guiding philosophy:

- save as many lives as we may, even our own

Within this repository model, "lives" should be understood broadly:

- human effort
- task and time identity
- meaningful claims
- continuity across devices
- recoverable workspace state
- the people whose privacy and agency the system should protect

This note is not an attempt to recreate legacy source-control systems. It is a
design note for a post-quantum, content-addressed, ancestor-bearing development
environment.

## Problem Statement

Today, multiple device-local folders may all be named `pudding`, while not
actually containing the same content or descending from the same known state.

That means the problem is not just storage. The problem is identity:

- which `pudding` is which
- what each one descends from
- what content is authoritative
- what has merely been replicated or materialized on a device

`loadngo` CAS already provides content-addressed blob storage. But the current
CAS layer is not yet a complete post-quantum repository model.

## Current State

### What exists now

- blob storage in [cas.rs](../data/src/cas.rs)
- fixed 32-byte `CasHash`
- current blob addressing by `blake3`
- reusable `pudding` manifest types in [pudding.rs](../data/src/pudding.rs):
  - tagged digest references
  - workspace manifests
  - transitional root manifests
  - signed root-manifest envelopes
- post-quantum signature tooling above the blob layer in
  [loadngo_cas.rs](../../sng-rusty/src/loadngo_cas.rs)
- root-manifest signing in
  [pudding_cas_ingest.rs](../data/src/bin/pudding_cas_ingest.rs), using
  `qcoin-crypto` key material
- initial post-quantum signed challenge-token tooling in
  [PQ_AUTHENTICATOR.md](PQ_AUTHENTICATOR.md)
- transitional tarball/signature workflows documented in
  [loadngo-cas-cloud-howto.md](../../sng-rusty/docs/loadngo-cas-cloud-howto.md)

### What does not exist yet

- algorithm-tagged CAS addresses at the storage layer
- final repository identity and trust-root policy for promoted `pudding`
  manifests
- automatic ancestor selection for new root manifests
- per-child repository file-manifest roots
- an explicit distinction between:
  - blob integrity
  - manifest authenticity
  - repository lineage
  - offsite replication state

So the current state is:

- CAS for storage
- native manifest structs for the first `pudding` root layer
- PQ signatures for signed root-manifest envelopes
- no full PQ-native repository lineage or replica policy yet

## Current Transitional Slice

The current implementation intentionally lands the smallest useful native
`pudding` layer without pretending the repository model is complete.

`pudding_cas_ingest` now emits:

- a workspace CAS manifest that records captured files and child repository
  state
- a transitional root manifest whose content root points at that workspace
  manifest
- an optional signed root-manifest envelope when `--signer-identity`,
  `--public-key`, and `--private-key` are supplied together

`pudding_cas_verify` now verifies:

- the signed root-envelope format
- the root manifest digest
- the PQ signature over the root manifest
- optional expected signer identity and trusted public key
- the root manifest's workspace-manifest CAS reference
- all workspace file blobs, unless `--manifest-only` is requested

This gives the workspace a signed, CAS-addressed root statement today. It is
still transitional because:

- child repository entries record branch/head/status, but not their own
  per-repository file-manifest roots
- ancestor links are accepted by the type model, but not selected automatically
  by the ingest command
- signing and verification keys are provided directly by path, not resolved
  through a trust-root store
- `CasHash` itself remains blake3-only even though manifest references are now
  algorithm-tagged

## Design Goals

The `pudding` repository model should be:

- content-addressed
- post-quantum signed
- ancestor-bearing
- algorithm-agile
- explicit about replication
- able to coexist with local authoring tools and working trees

This means the CAS lineage should do more than prove that bytes existed. It
should help preserve:

- work that matters
- ancestry that matters
- privacy that matters
- recovery paths that matter

The intent is:

- local working directories remain useful materializations
- the authoritative integration truth becomes signed CAS lineage

## Core Distinction

There are four different concerns:

1. blob identity
2. manifest identity
3. repository lineage
4. replica/materialization state

These should not be collapsed into one field or one file.

## Blob Identity

Individual files or content chunks should be stored as CAS blobs.

Requirements:

- stable content-addressed identifiers
- verification on read
- optional storage-layer compression
- no requirement that a whole repository be packed into one archive

### Important note

Blob integrity and post-quantum authenticity are different concerns.

A hash gives:

- content identity
- corruption detection
- deduplication

A signature gives:

- authorship or promotion authority
- authenticity of the root statement

The blob layer should therefore remain hash-addressed, while repository roots
must be signed separately.

## Hash Agility

The current `CasHash` type in
[cas.rs](../data/src/cas.rs) is fixed to a 32-byte
digest and implicitly tied to one algorithm.

That is not sufficient for the desired PQ posture.

The repository model should move to algorithm-tagged addresses, for example:

- `blake3-256:<hex>`
- `sha3-256:<hex>`
- `shake256-512:<hex>`

### Why

- avoids silently baking one hash forever into the repository identity model
- allows migration without pretending old content never existed
- makes verification rules explicit
- lets stronger digest sizes be adopted for root or manifest identities if desired

## Repository Identity

`pudding` should become a repository identity, not merely a folder name.

That means a root manifest should declare at minimum:

- repository id
- manifest format version
- root manifest hash
- signer identity
- signature scheme
- creation timestamp
- ancestor reference
- child references
- inclusion policy reference
- notes

Suggested repository id:

- `pudding`

But the authoritative identity is not the string alone. It is the signed lineage
of manifests associated with that repository id.

## Ancestor Model

Each promoted `pudding` snapshot should name its ancestor explicitly.

Suggested fields:

- `repository_id`
- `ancestor_manifest`
- `manifest_hash`
- `content_root`
- `signer`
- `signature_scheme`
- `signature`

This allows statements like:

- this device's `pudding` materialization descends from ancestor `X`
- this new promoted state supersedes ancestor `Y`

Without this, multiple folders named `pudding` remain ambiguous.

## Content Root Model

The repository should not start from a single tarball as its core abstraction.

Instead, the content root should be a structured object that references:

- workspace-level metadata files
- child repository entries
- optional media or large-asset references

Suggested shape:

- root manifest
  - workspace metadata entry
  - child repo entries
  - inclusion policy entry
  - optional media set entry

Each child repo entry should contain:

- repo name
- branch or local channel name if relevant
- local head identifier if available
- file manifest root
- local divergence summary

## Child References

The `pudding` root should not erase the existence of child projects.

Instead it should reference them explicitly:

- `sng-rusty`
- `loadngo`
- `qcoin`
- `entitlement-achievement-blockchain`
- optional historical/legacy children such as `loadngo-cpp`

Each child reference should distinguish:

- source identity
- included file manifest root
- working-state metadata
- optional local patch/delta records

This keeps the parent/child model intact without making child-local history the
only truth layer.

### Workspace declaration

The set of child repositories should be explicit, not hardcoded inside a tool.

Near-term, `pudding` should declare its children in a root workspace config such
as:

- [pudding.workspace.ron](../../pudding.workspace.ron)

That declaration should identify:

- child name
- child path
- whether the child is required or optional
- inclusion policy for that child

This avoids two failure modes:

- treating one machine's local folder layout as the definition of `pudding`
- accidentally ingesting or omitting children based on hidden binary constants

## Materializations And Replicas

The system must distinguish:

- authoritative promoted manifests
- device-local materializations
- offsite replicas

### Device materialization

A local folder on a machine is a materialization:

- may be incomplete
- may contain unpromoted changes
- may lag the latest promoted ancestor

### Replica

An offsite copy such as a Google Drive relay is a replica:

- not the source of truth by itself
- stores promoted manifests and referenced blobs
- may lag current local work

The manifest model should eventually support recording replica observations such
as:

- provider
- replica path
- uploaded timestamp
- remote checksum or object id

## Compression Model

Compression should be treated as a storage or transport concern, not as the
repository identity itself.

That means:

- blobs may be stored compressed internally
- transport packages may be compressed for relay efficiency
- but the content identity model should not depend on "one tar.gz archive"

The current tarball/signature workflow remains acceptable as a transitional
export path, but not as the long-term repository substrate.

## Signature Model

Repository roots should be signed with a post-quantum signature scheme.

Current tooling already supports PQ signatures in
[loadngo_cas.rs](../../sng-rusty/src/loadngo_cas.rs).

Long-term expectations:

- promoted root manifests are PQ-signed
- ancestor links are covered by the signature
- signer identity is explicit
- verification is possible independently of the local machine that created the snapshot

## Transitional Model

Until the native repository-root format exists, the project may continue using:

- CAS blob storage for raw content
- exported JSON or RON manifests
- PQ signatures over those manifests
- offsite relay of manifest plus referenced content

But that should be understood as transitional, not final.

## Migration Path

Recommended order:

1. Stabilize the current signed-root slice.
2. Add per-child repository file manifests and fill each child
   `file_manifest_root`.
3. Add explicit ancestor discovery and promotion rules.
4. Introduce algorithm-tagged CAS hash identities at the storage layer.
5. Add repository trust-root policy and signer rotation rules.
6. Add replica records for Google Drive or other offsite relays.
7. Keep tarball export only as a compatibility fallback.

## Next Evolution Steps

### 1. Stabilize the current signed-root slice

The current slice should remain small and testable:

- `pudding_cas_ingest` emits a workspace manifest, transitional root manifest,
  and optional signed root envelope
- key files are hex-encoded `qcoin-crypto` key material, matching
  `loadngo_pq_auth keygen`
- smoke tests should cover unsigned ingest, signed ingest, and verification of
  the envelope payload
- generated manifests should remain deterministic for identical inputs

This step is about making the landed format safe to build on before widening
the repository semantics.

### 2. Split child repositories into their own file manifests

Each child repository entry should stop being only branch/head/status metadata.
It should point at a child file-manifest root that captures the included files
for that child.

That gives the parent root a clean shape:

- parent `pudding` root manifest
- workspace manifest
- child repository metadata
- child repository file-manifest roots
- blob references below those roots

This also prevents one child repository's changing working state from being
confused with the identity of another child or the parent repository itself.

### 3. Define ancestor discovery and promotion policy

The type model can already carry ancestor references, but the ingest command
does not yet decide which prior manifest is authoritative.

The next policy work is:

- accept an explicit `--ancestor-manifest` path or CAS reference
- verify the ancestor hash before signing a descendant
- support a local pointer to the latest promoted root
- reject accidental promotion from an unverified or mismatched ancestor
- keep unpromoted local materializations separate from promoted lineage

Without this step, signed roots prove snapshot authenticity but not repository
continuity.

### 4. Make CAS addresses algorithm-agile

Manifest references can already carry digest algorithm tags, but the storage
layer still uses fixed blake3-backed `CasHash` values.

The migration should introduce a tagged CAS address beside the existing type,
then migrate call sites deliberately:

- preserve existing blake3 content without rewriting it
- make new manifest/root references include algorithm labels
- add verification code that rejects unknown or disallowed algorithms
- keep compatibility readers for current `CasHash` blobs during the transition

### 5. Add trust-root and signer policy

Direct key-file signing is useful for bootstrapping, but promoted roots need a
separate trust policy.

The trust-root store should define:

- accepted signer identities
- accepted PQ signature schemes
- public keys and key epochs
- key rotation and revocation records
- verification outcomes such as accept, quarantine, or reject

This keeps repository authority separate from whichever local file path happened
to contain a signing key during ingest.

### 6. Record replicas as replicas, not authority

Google Drive or other relays should store signed manifests and referenced blobs.
They should not become the source of truth by path alone.

Replica records should describe:

- provider and remote object identity
- upload timestamp
- referenced manifest hash
- optional remote checksum or generation id
- last verified timestamp

The signed root lineage remains authoritative; replicas are transport and
recovery observations.

### 7. Extend verification and add repair commands

The long-term command set should make recovery mechanical:

- verify a signed root envelope, which `pudding_cas_verify` now starts
- verify all child manifests and blobs below that root as child file manifests
  land
- explain missing, corrupt, unknown, or untrusted content
- materialize a verified root into a working directory
- repair a partial materialization from local or replica CAS stores

This is the point where `pudding` becomes more than backup metadata: it becomes
an inspectable, recoverable, PQ-authenticated workspace lineage.

## Immediate Next Steps

The next implementation steps implied by this note are:

1. Add CLI and tests for explicit ancestor manifest selection.
2. Split the workspace manifest into parent and per-child file manifests.
3. Add a trust-root file for accepted signer identities and PQ public keys.
4. Wire trust-root policy into `pudding_cas_verify`.
5. Introduce tagged CAS storage addresses while retaining `CasHash`
   compatibility readers.
6. Add replica records for offsite providers.
7. Keep full-workspace ingestion intentional; large children such as `zhoenus`
   should use explicit include policy and lightweight smoke fixtures by default.

## Related Notes

- [ARCHITECTURE.md](ARCHITECTURE.md)
- [loadngo-cas-cloud-howto.md](../../sng-rusty/docs/loadngo-cas-cloud-howto.md)
- [RELEASE_VERSIONING_AND_INTEGRITY.md](RELEASE_VERSIONING_AND_INTEGRITY.md) —
  open proposal to extend this signed-root-manifest pattern to player-facing
  release artifacts (APKs, IPAs, itch.io Linux builds), not yet implemented.
