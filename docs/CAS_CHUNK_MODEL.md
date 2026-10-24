# Chunk-addressed content and materialized files

Status: design exploration, 2026-10-24. This document records a direction for
Loadngo CAS and the planned Pudding repository model. It proposes no immediate
format or implementation change.

## Motivation

Files and directories are useful interfaces for people and existing tools, but
they need not be the lowest-level storage units. A file-oriented CAS stores and
deduplicates whole files. That works well for preservation and simple recovery,
but a small edit to a large file creates a new whole-file object. A chunk store
can retain unchanged regions and transfer only new content. Above that, a
repository manifest can give a durable identity to a complete workspace and
its history, regardless of how its bytes are physically packed.

This suggests a layered model:

1. **Content identity:** immutable byte chunks addressed by a cryptographic
   digest.
2. **Reconstruction:** ordered chunk references describe file byte streams;
   tree manifests map paths and metadata to those streams.
3. **Repository identity:** signed, ancestor-bearing manifests describe
   workspace state, authorship, changes, and trust decisions.
4. **Materialized views:** ordinary files and directories are checked-out
   views for editors, compilers, and existing applications.
5. **Optional structured reuse:** format-aware recipes can describe reusable
   platform scaffolding, subject to exact reconstruction and verification.

These layers separate physical deduplication from semantic identity. Equal
bytes can be stored once without deciding that two files, tasks, builds, or
repositories mean the same thing.

## Current Loadngo and Pudding state

Loadngo Archive CAS currently stores immutable whole-file objects addressed by
BLAKE3-256. Equal files share an object, and archive manifests preserve paths,
parent roots, and change records. It streams large inputs and verifies objects
by hashing them. The packet-oriented `data::cas::CasStorage` is a separate
small-object network CAS; this design note does not propose changing its wire
format. See [Archive CAS](ARCHIVE_CAS.md).

The Pudding CAS design adds workspace/root manifests and post-quantum signed
root envelopes, but describes the implementation as transitional. Automatic
ancestor selection, per-child repository file-manifest roots, final trust-root
policy, and algorithm-agile storage addresses remain open. See
[Pudding CAS PQ Model](PUDDING_CAS_PQ_MODEL.md).

The next source-control layer should therefore focus on repository identity,
lineage, signed changes, and reconciliation. It can use CAS objects underneath
without making storage layout itself the history or trust model. The
[Reconciliation design](RECONCILIATION.md) already distinguishes content,
placement, origin, and recorded decisions.

## Xet as a storage reference

[Hugging Face Xet](https://huggingface.co/docs/xet/index) is a useful reference
for chunk-level storage and transfer. Its protocol specifies deterministic
content-defined chunking, hashing, packing chunks into xorbs, file reconstruction
from chunk ranges, deduplication, and upload/download APIs. In its documented
scheme, chunks target about 64 KiB, with 8 KiB minimum and 128 KiB maximum
sizes. Content-defined boundaries allow unchanged regions after an insertion
or deletion to retain their chunk identities. See the [chunking
specification](https://huggingface.co/docs/xet/chunking) and [file
reconstruction](https://huggingface.co/docs/xet/file-reconstruction).

The conceptual overlap is substantial: both approaches use immutable,
content-addressed data and manifests to reconstruct larger objects. The
important scope distinction is that Xet specifies a storage and transfer
protocol; it does not by itself supply the repository lineage, authorship,
post-quantum trust, or conflict decisions in the Pudding design. Its formats
and algorithms are candidates for study, not a decision to adopt its protocol.

## Files as a view over chunks

A possible chunk-native representation of a file is an ordered list of chunk
references plus its byte length and optional metadata. A directory tree then
maps canonical relative paths to file descriptions and child trees. A
workspace root can reference one or more repository trees. Reconstruction
concatenates the referenced bytes and verifies the resulting file digest.

The user-facing checkout remains a normal filesystem tree. It is created from
the manifest and chunks, and edits are re-ingested to produce a new immutable
root. This keeps compatibility with current programs while allowing storage,
transfer, replication, and history to operate on shared chunks. The manifest
root, rather than a particular disk layout, becomes the stable identity.

Content-defined chunking is a promising default for arbitrary binary data.
Fixed-size chunks are simpler but insertions can shift every later boundary;
content-defined chunking tends to confine that change to nearby chunks.
Chunk size and hashing rules must be deterministic and versioned so independent
implementations agree. Very small files may remain whole objects to avoid
metadata overhead. Packing chunks into larger immutable containers can improve
I/O locality, but reconstruction must still address verified chunk identities
and avoid excessive fragmentation or tiny range fetches.

## Exact chunks and structured templates

There are two distinct kinds of reuse:

- **Exact chunk reuse** shares only byte-identical chunks. It is generic,
  deterministic, and preserves the ordinary cryptographic content identity.
- **Structured template reuse** recognizes a format and represents an artifact
  as a known base plus parameters or replacements. This can save more space
  when artifacts share scaffolding, but depends on a parser, canonical
  parameterization, and a reproducible reconstruction rule.

Portable Executable (PE) files illustrate the opportunity and the limits. PE
images contain headers, sections, tables, and potentially relocation and
certificate data. Some bytes may repeat across a family of builds, while other
fields depend on linking, image layout, signing, timestamps, or build inputs.
The [Microsoft PE/COFF specification](https://learn.microsoft.com/en-us/windows/win32/debug/pe-format)
documents these structures, including section layout and relocations. A
platform template must not assume that every executable for an OS shares
replaceable bytes. It should be an explicit recipe for a defined toolchain and
format version, with parameters and output checked against the reconstructed
artifact hash. The same principle applies to Mach-O, ELF, package formats, and
model checkpoints.

Templates should be an optimization or a higher-level artifact type, not a
substitute for the exact content representation. If a recipe is missing,
unsupported, or untrusted, the artifact must still be recoverable from its
ordinary chunks. A signed manifest should identify the recipe, its version,
inputs, and output digest whenever the recipe is used as provenance.

## Repository history above storage

Chunk references solve byte reuse; they do not decide what a change means.
Repository history should continue to record immutable roots and explicit
ancestry. A proposed change can identify its parent root, resulting tree root,
actor, time, and declared changes. Signatures establish who approved a root;
they do not prove that the content is safe, correct, or semantically equivalent.
When two devices diverge, reconciliation should preserve both origins and
record the resolution instead of silently selecting one tree.

This keeps repository identity independent of whether files are currently
stored as whole objects, CDC chunks, packed containers, or recognized
templates. Storage improvements can then be introduced without rewriting the
meaning of existing history.

## Thermal-aware work scheduling

ComfyUI is a useful workload example. Its core runs as a Python/PyTorch
application with a browser frontend; the official project documents launching
the backend with `python main.py` and ships compiled frontend assets. Node.js
may be part of frontend development, but it is not the image-generation runtime
([ComfyUI project](https://github.com/Comfy-Org/ComfyUI)). Image generation can
heavily load the GPU and memory system. This document records an observation
that ComfyUI did not appear to offer thermal-aware scheduling; that observation
is not a complete audit of every extension or desktop build.

Loadngo applications and long-running local workflows can make scheduling
decisions around work units such as queued generations, model loads, or
background indexing. A thermal-aware policy could defer optional work, lower
concurrency, or pause between jobs when the host reports thermal or power
pressure. It should use host abstractions and supported fallbacks, keep
correctness independent of sensor availability, and remain event-driven or
coarsely sampled. Hardware/OS thermal throttling remains a safety backstop,
not evidence that application work is paced appropriately. Any claim of
thermal safety needs representative idle and active measurements on the target
hardware; see [Thermal Awareness](THERMAL_AWARENESS.md).

## Design questions before implementation

- Which digest algorithms and format-version rules should identify chunks,
  reconstructed files, trees, and signed repository roots?
- Should CDC parameters follow an existing interoperable specification or be
  selected and versioned for Loadngo workloads?
- What container/index layout provides good sequential I/O without making
  random reconstruction expensive?
- How should garbage collection find references across repository roots,
  replicas, pinned artifacts, and retained history?
- What authorization boundary applies to deduplication across users or
  repositories? Global dedup can reveal that another party already possesses
  particular content through timing or size behavior.
- Which metadata belongs in manifests, and which paths, names, or build details
  need encryption or local-only handling?
- Which structured formats justify templates, and how can each recipe be
  sandboxed, versioned, reproducible, and independently verified?
- How should a scheduler respond to platform thermal signals while respecting
  user priority, deadlines, cancellation, and interactive work?

## Suggested exploration order

1. Measure representative workspace and model-artifact snapshots to establish
   whole-file deduplication, CDC deduplication, metadata, CPU, and I/O costs.
2. Prototype deterministic chunking and file reconstruction in an isolated
   tool or format version, retaining current Archive CAS readers and writers.
3. Compare exact chunks with a small, explicit structured-template prototype
   for a known reproducible artifact family; verify byte-for-byte output.
4. Design tree manifests and repository lineage independently of physical
   packing, including signatures, trust, reconciliation, privacy, and GC.
5. Prototype thermal-aware queue control around discrete jobs, measure it on
   target hardware, and keep it separate from content identity.

These are investigation steps, not commitments to replace files, Archive CAS,
or existing source-control tools.
