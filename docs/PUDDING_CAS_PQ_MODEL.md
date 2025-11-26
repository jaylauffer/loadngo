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
- post-quantum signature tooling above the blob layer in
  [loadngo_cas.rs](../../sng-rusty/src/loadngo_cas.rs)
- initial post-quantum signed challenge-token tooling in
  [PQ_AUTHENTICATOR.md](PQ_AUTHENTICATOR.md)
- transitional tarball/signature workflows documented in
  [loadngo-cas-cloud-howto.md](../../sng-rusty/docs/loadngo-cas-cloud-howto.md)

### What does not exist yet

- algorithm-tagged CAS addresses
- a first-class `pudding` repository identity
- signed ancestor-bearing root manifests inside the CAS model itself
- an explicit distinction between:
  - blob integrity
  - manifest authenticity
  - repository lineage
  - offsite replication state

So the current state is:

- CAS for storage
- PQ signatures for some exported bundles
- no full PQ-native repository lineage yet

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

1. Introduce algorithm-tagged CAS hash identities.
2. Define the `pudding` root manifest schema.
3. Make the root manifest reference child manifests and blob roots instead of tarballs.
4. Sign promoted root manifests with PQ signatures.
5. Add replica records for Google Drive or other offsite relays.
6. Keep tarball export only as a compatibility fallback.

## Immediate Next Steps

The next implementation steps implied by this note are:

1. make `CasHash` algorithm-agile
2. add a native `pudding` manifest type in `loadngo`
3. sign that manifest with PQ tooling
4. separate:
   - local materialization state
   - promoted ancestor state
   - offsite replica state

## Related Notes

- [ARCHITECTURE.md](ARCHITECTURE.md)
- [loadngo-cas-cloud-howto.md](../../sng-rusty/docs/loadngo-cas-cloud-howto.md)
