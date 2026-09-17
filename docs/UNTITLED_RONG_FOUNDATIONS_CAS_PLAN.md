# Untitled SSD preservation and Rong Foundations CAS plan

## Status and decision boundary

**Planning only.** This document authorizes no copy, ingest, checksum scan,
reformat, mount-mode change, task-plane request, QCoin anchor, or deletion.
The external SSD remains the read-only source of truth until Jay explicitly
approves a later gate.

The goal has two deliberately separate outcomes:

1. preserve the SSD's existing contents faithfully and privately; and
2. create a small, consciously curated **Rong Foundations** registry that can
   guide local creative work without turning a personal archive into an
   unreviewed model corpus.

The second outcome is a reference and governance layer over preserved content.
It is not permission to train on, publish, prompt from, or distribute every
object in the archive.

## Read-only findings

The source is `/Volumes/Untitled`, an approximately 1.9 TiB NTFS volume mounted
read-only on the Mac mini. It reports approximately 1.1 TiB used. The root is a
mixed historical Windows installation, not a clean media drive. It includes:

- personal, financial, identity, account-recovery, and SSH material;
- Windows system, application, recovery, hibernation, and installer material;
- code, game/project trees, archives, and third-party software/model material;
- creative writing, music/audio, image/3D, and Rong/Zhoenus-adjacent work.

This makes a raw "ingest everything and label it Rong" approach unacceptable.
It would mix sensitive material, unknown third-party rights, executable code,
and creative candidates into one indistinguishable pool.

There is also a physical-capacity conflict. The external SSD cannot retain its
approximately 1.1 TiB existing logical content and the planned K3 payload of
about 1.52 TiB at the same time. The Mac mini's current data volume has only
about 160 GiB free and is not a staging destination.

## Existing Loadngo foundation and its gap

The `data` crate already provides a BLAKE3-256 CAS, verified reads, workspace
manifest types, signed root manifests, and the `pudding_cas_ingest` /
`pudding_cas_verify` tools. Those tools are appropriate evidence for small,
Git-oriented workspace snapshots; they are not yet safe archive tooling:

- `CasStorage::add_file` reads a whole file into memory;
- object sizes and offsets are `u32`, so objects above 4 GiB are unsupported;
- the existing workspace ingest relies on Git-visible files and records paths
  in a plaintext JSON manifest;
- it has no encrypted metadata policy, archival classification, resumable
  large-file ingest, or independent restore test.

K3's large shard/trunk files require one stored byte sequence plus reliable
positioned reads. A copy beside the CAS would defeat the capacity budget.

## Architecture: three planes, not one bucket

```text
Read-only Untitled SSD
        |
        |  explicit approval only
        v
Encrypted preservation CAS  <---->  encrypted second copy / off-site recovery
  complete logical archive            (independent verification)
        |
        |  human classification and explicit inclusion
        v
Rong Foundations registry
  signed, minimal, access-tiered references
        |
        |  separately approved assets only
        v
Local creative work / runtime asset review

After preservation is proven, Untitled may become a separate high-capacity
Loadngo CAS node for K3. It is not the only preservation copy.
```

### 1. Preservation CAS

This is the complete logical archive. It must be encrypted at rest, private,
and independently restorable. It preserves bytes and enough metadata to locate
them, but does not make the material searchable or available to workers by
default.

The canonical full path, filenames, timestamps, and sensitive classifications
stay inside the encrypted archive manifest. They must not be committed to Git,
printed into task traffic, put into QCoin metadata, or exposed through a shared
CAS index. A public/auditable receipt, if Jay wants one later, contains only a
version, count/size summary, and a deliberately approved commitment to the
encrypted manifest.

### 2. Rong Foundations registry

This is a small curated ledger, not a duplicate content store. Each entry refers
to an archived object and records the human decision that permits its use.

Every entry has one of these access classes:

| Class | Meaning | Allowed use |
|---|---|---|
| `restricted-preservation` | Sensitive, personal, identity, account, or secret material | Preserve only; no indexing, generation, sharing, or task-worker access |
| `system-recovery` | Windows, installers, drivers, recovery, or executable material | Preserve for restoration only; never execute from CAS; no creative use |
| `quarantined-third-party` | Model, code, archive, or media with unclear rights/intent | Preserve and review; no training, redistribution, or creative inclusion |
| `private-reference` | Creator-owned material requiring Jay's per-item direction | Human review reference only; no automatic ingestion into prompts or training |
| `licensed-creative` | Rights and intended use have been recorded | May enter a defined local project workflow after review |
| `release-ready` | Explicit project, safety, and release approval exists | May follow normal asset promotion rules |

The first Rong Foundations collection should favor intentionally selected music,
writing, visual/3D studies, game notes, and project sources whose ownership and
intended use Jay confirms. It must record why an item is representative (for
example, a theme, a material practice, a world-building idea, or a musical
motif), rather than infer that from a folder name.

### 3. K3 performance CAS

Once the source has two verified preservation copies and Jay explicitly approves
the cutover, the reformatted SSD can hold K3 objects only once as large CAS
blobs. This performance node may hold a thin, non-sensitive reference to the
Rong Foundations registry, but it must not be mistaken for the private archive.

## Capacity and authority gates

No reformat can occur before all gates below are satisfied.

1. **Preservation target named.** For a logical archive, provide at least one
   encrypted external destination with more than the current source use plus
   operational headroom; a nominal 2 TB target is the minimum, while 4 TB is
   the practical recommendation. A raw device image, if desired, needs a
   separate target with at least the full source capacity and should be planned
   as a forensic option, not assumed.
2. **Independent recovery copy named.** Before source erasure, maintain a
   second independently verified archive copy or a tested off-site equivalent.
   The read-only source counts only until cutover; it is not a recovery copy
   after a reformat.
3. **Key custody defined.** Jay names the encrypted-volume/key holder and the
   recovery procedure. No encryption key, recovery code, or secret-file hash is
   committed to Loadngo, QCoin, a task receipt, or an agent transcript.
4. **Scope approved.** Jay decides whether preservation means logical files
   only (the default) or includes a whole-device forensic image.
5. **Capacity strategy approved.** Choose one of:
   - archive first on a new preservation drive, then repurpose Untitled for K3;
   - a larger dedicated archive CAS that holds both archive and K3; or
   - keep Untitled as archive-only and defer K3 storage there.

Because approximately 1.1 TiB plus approximately 1.52 TiB exceeds this SSD's
usable capacity, there is no safe fourth option that stores both complete sets
on Untitled.

## CAS-v2 work required before source ingest

Extend the existing `loadngo-data` CAS rather than replacing its BLAKE3 address
scheme. The archive path needs a versioned large-object layer with these
properties:

1. **Streaming ingest:** hash and copy from a bounded buffer, never with
   `fs::read`; support `u64` size and offset values.
2. **Atomic publish:** write a temporary object, `fsync` the object and parent,
   verify its BLAKE3 hash and size, then atomically publish its descriptor.
   Crash recovery may retain only explicitly marked incomplete temporary data.
3. **Positioned reads:** expose `read_range(hash, offset: u64, len)` so K3 can
   load a shard window without materializing a second whole-file copy.
4. **Resumable transfer:** record chunk-level progress in a private journal;
   revalidate every completed chunk on resume. The final object hash remains the
   BLAKE3 hash of the original byte stream.
5. **Archive manifest:** use a separate, encrypted `archive-cas-v1` manifest
   for path, source metadata, classification, and object references. Do not
   overload the current Git-workspace manifest format.
6. **Integrity:** verify object length and full BLAKE3 digest after ingest;
   verify a deterministic sampled restore set from the second copy; sign only
   the approved root commitment after those checks pass.
7. **Safety:** treat source files as bytes. Do not open executables, activate
   macros, import browser profiles, mount disk images, or execute source code
   while preserving them.

The v2 design needs fixture tests for multi-GiB logical objects without needing
multi-GiB test fixtures, interrupted ingest, hash mismatch, crash recovery,
range-read boundaries, deduplication, encrypted manifest separation, and a
K3-style positioned-read adapter.

## Staged execution plan

### Phase 0 — preservation record (read-only)

Create a private, encrypted inventory with no content previews: volume identity,
mount mode, top-level classification, file count/byte totals by class, unreadable
paths, and a timestamp. Do not record sensitive names in the Git plan or task
plane. This phase produces an unsigned local inventory receipt only.

Success: the source is still read-only; no files have been copied or altered;
the inventory distinguishes `restricted`, `system`, `quarantine`, and
candidate-creative material.

### Phase 1 — CAS-v2 implementation and clean-room test

Implement the large-object, private-manifest, and range-read changes against
synthetic fixtures only. Test the K3 positioned-read contract with a generated
large logical fixture. This is a normal Loadngo code change with unit and
cross-platform checks; it does not touch Untitled.

Success: bounded-memory ingest, `u64` addresses, full verification, resume,
and positioned-read tests pass; no source object was read.

### Phase 2 — encrypted destination and pilot

Provision the approved encrypted archive destination and create a pilot archive
from a tiny set of explicitly selected, non-sensitive material. Compare source
and CAS hashes, restore to a separate test location, and compare hashes again.
Record the object/manifest root privately.

Success: source-to-CAS and CAS-to-restore hashes match; destination encryption,
key recovery, access policy, and failure recovery are demonstrated.

### Phase 3 — full logical preservation

Ingest all logical source files into the encrypted preservation CAS with
classification assigned before any derived use. Resume safely after interruptions
and retain a private error list for unreadable items. Run a second full
verification pass plus deterministic restore samples from an independent copy.

Success: every readable source file is accounted for exactly once as preserved,
deduplicated, intentionally excluded with a reason, or recorded as unreadable;
the second copy verifies independently.

### Phase 4 — Rong Foundations curation

Jay reviews candidate-creative records and creates a small registry of explicit
entries with access class, rights/intent note, project relevance, and reviewer.
No generated output and no unknown-third-party material can be elevated by
default. The registry can reference object IDs, but its sensitive mappings stay
in the encrypted manifest.

Success: every foundation item has an affirmative human inclusion decision;
the registry has no `restricted-preservation` or `quarantined-third-party`
entry marked usable.

### Phase 5 — K3 cutover

Only after Jay approves the Phase 3 evidence and confirms two recoverable
preservation copies, unmount/reformat Untitled as the K3 performance CAS. Ingest
the K3 shard/trunk objects once through CAS-v2, then run the K3 engine against
CAS range reads and verify oracle output and full hashes.

Success: K3 bytes are stored once, positioned reads work, no adjacent import
copy exists, and the legacy archive remains independently restorable after the
source SSD is repurposed.

## Loadngo Task use

These phases are meaningful work but must not use the task plane to transmit
archive content, plaintext filenames, keys, or sensitive hashes. Suitable
task-plane artifacts are bounded code-test receipts, destination health proofs,
and private-manifest commitment verification. A worker is assigned only after a
direct `TaskAccept`; a `TaskResult` is checked against the phase criteria; only
then may a `TaskAck` close it. A QCoin anchor, if Jay requests one, occurs only
after positive acknowledgement and must commit to an approved non-sensitive
receipt—not to archive data itself.

## Immediate next decision

Jay needs to choose the preservation topology and confirm the scope (logical
archive versus forensic image). Until then, the only safe next action is Phase 0
read-only inventory work. No one should reformat Untitled, copy its contents to
the Mac mini, or point a model/trainer at it.
