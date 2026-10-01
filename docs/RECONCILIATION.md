# Reconciliation: one model for Task sync and archive versions

Status: design, 2026-10-01. Nothing here is implemented yet. It follows from
"Lessons From The First Edit Of A Signed Archive" in
[PUDDING_CAS_PQ_MODEL.md](PUDDING_CAS_PQ_MODEL.md).

"Task" in this note means the task-list entities and their synchronisation
between machines: the C++ code in `loadngo-cpp/Task` (`Data/Undo.*`,
`Data/Sync.*`, `Task/Network/TaskSynch.*`), its Rust port in `data`
(`entity`, `sync`) and the legacy frames in `network` (`RequestMoveChain`).
It is not the TaskRequest/TaskOffer work protocol
([TASK_OFFER_PROTOCOL.md](TASK_OFFER_PROTOCOL.md)), though signing below
connects the two.

## The problem both have

Items arrive from separate origins: two machines, two captures of a drive,
two agents. Then:

- Two items from separate origins may be the same item, or may differ only
  in arcane details. Deciding which is the primary problem.
- Identical bytes do not make two items the same: two empty files, two
  copies of a licence, the same template filled in twice.
- One item changes over time on each side: it is edited, moved, deleted.
- The decision, once made, must be recorded, with who made it and why,
  and both origins must stay in the history.
- Data may later be dropped (a retired version, a cleared undo entry)
  while its record stays.

## What each has today

| Concept | Task (C++) | Archive CAS |
|---|---|---|
| Where it came from | origin id (`CreateTextOriginId(title, created)`), machine id, user id | archive id and source label per capture; nothing per entry |
| Its version | entity `id`, changed by every edit | manifest root per archive version; per entry, `(path, object)` |
| Its content | property id (hash of properties), children id (`GenerateIdForChildren`), entity hash (SHA-512) | object hash (BLAKE3 of the bytes); no hash per directory |
| Where it sits | parent origin id | path |
| Proposing that two are the same | skeletal id (`Task::GetSkeletalId`, hash of the exact title; its comment notes "Take out trash" and "take out trash" do not match). Sync itself matches by origin id only | byte equality only |
| Their differences | `Discrepancy`: `Moved`, `Children`, `Properties`, `Wanted`, `Deleted` | none |
| The decision | `Merged` (which side's property was kept), consolidated id agreed by participants | none |
| Deletion | `Deleted` record holds the contents; `Undo::Clear()` drops them and keeps the record | a retired version keeps its manifest and signature; its unique objects go (since `12f03cb6`) |
| History | per-machine undo journal; move chains consolidated since the last concluded sync (`ConsolidateMovesSince`, `MakeCourse`) | `supersedes_archive_root`: one parent; merges and unpacks noted in sidecar logs |
| Hashes | FNV-1a 64-bit ids, SHA-512 entity hash | BLAKE3-256 |

Each side has half of it. Task records decisions but proposes sameness
crudely and only within one origin. The CAS identifies content precisely
but cannot propose anything beyond equal bytes, or record any decision.

One defect in the Rust port matters here: `Entity::property_hash` and
`Sync::hash` fold a `HashMap` in iteration order, which Rust randomises per
map, so the same entity hashes differently in two processes. Anything that
crosses machines must hash canonical bytes.

## The model

Everything below is a content-addressed object in the CAS (BLAKE3 over
canonical bytes), so it is deduplicated, verifiable and signable the same
way archived files are.

### Origin

Where an item was first recorded: the capture or creation, the machine, the
user or agent, the time. An origin is permanent and is never merged away.

- A task's origin is what `CreateTextOriginId` makes today.
- An archived file's origin is implicit: the capture (archive id and the
  root it was captured in) plus its path at capture. No per-entry record is
  written at ingest, so a capture of 500,000 files costs nothing extra.

### State and placement

An item's **state** is its content: a task's properties and children, a
file's object hash. Its **placement** is where it sits: a task's parent, a
file's path. A move changes placement only; an edit changes state only. This
is the split Task already makes between property id and parent origin id,
and the one the CAS lacks when a path is all that ties an entry across
versions.

### Records

One record per change, each naming its subject (an origin, or a set of
origins judged the same; see `Same`), its actor (user, machine or agent),
time and reason:

| Record | Meaning | Task today | Archive today |
|---|---|---|---|
| `Created` | a new item, with its state and placement | entity creation | `archive_cas_add` |
| `Changed` | state from A to B | property edit | (a changed file re-captured) |
| `Moved` | placement from A to B | `Moved` | (rename: shows as remove plus add) |
| `Deleted` | removed; its state is kept until retired | `Deleted` | `archive_cas_remove` |
| `Derived` | one item becomes others | (a task split) | zip unpack |
| `Same` | separate origins are one item, and which value of each differing detail is kept | `Merged`, consolidated id | none |
| `Distinct` | a proposed match is not the same item, so it is not proposed again | none | none |

`Same` is the decision at the centre of this. It keeps every origin, names
the item they now form, and records each resolved difference (as `Merged`
records the kept property today). Later records may name any of the joined
origins. `Distinct` is its counterpart: identical bytes that are different
things, or a title match that is a different task.

A removed directory is one `Deleted` record for the subtree, not one per
file; that needs directory nodes (lesson 3 in the PQ model).

### Consolidation

A signed object that closes a set of records: its parent consolidations
(one or more), the records since them, and the resulting tree. It is both:

- an **archive version**: today's manifest becomes the tree of a
  consolidation, and the change record lesson 2 asks for is its records;
- a **sync point**: what `consolidated_id` names today. Each participant
  signs the same consolidation, so agreement is verifiable rather than a
  shared id.

More than one parent replaces `supersedes_archive_root`: the merge of seven
archives into `loadngo-archive` is a consolidation with seven parents, and
two machines that diverged are reconciled by a consolidation with both.

Consolidating since a point (`ConsolidateMovesSince`) becomes: records
between two consolidations may be collapsed (three moves become one) in the
newer one, while the older consolidations stay retrievable unless retired.

### Proposals and discrepancies

A **proposal** says two items may be the same, with its evidence. Proposers
are pluggable; none decides:

- equal content (object hash or state hash): strongest evidence, still not
  a decision;
- skeletal keys per kind: a task's title folded for case and Unicode form
  (the C++ comment's ICU note); a text file normalised for line endings and
  trailing whitespace; name and size;
- known relations: a zip and its unpacked members, a capture and an earlier
  capture of the same source path;
- same origin (what Task sync does today): two versions of one item.

For each proposal, the **discrepancy** lists how the two differ: state
details (Task's `Properties`), children (`Children`), placement (`Moved`),
present on one side only (`Wanted`), deleted on one side (`Deleted`).
Resolving it writes records: `Same` with the kept details, `Distinct`, or
`Changed` and `Moved` to bring one side into line.

### Who signs

A consolidation is signed by its author's key (an agent's own key, or a
person's) and, where the change needs approval, by the approver's. In the
work protocol, an agent's `TaskResult` can carry the consolidation it
produced, and the positive `TaskAck` is the approver's signature on it.

## How today's operations map

| Operation | Records | Consolidation |
|---|---|---|
| `archive_cas_ingest` | none (origins implicit) | new, no parent |
| `archive_cas_add` | `Created` per named path | one parent |
| `archive_cas_remove` | `Deleted` per named path | one parent |
| `archive_cas_exclude` | `Changed` (unreadable to excluded) | one parent |
| `archive_cas_unpack` | `Derived` per zip | one parent |
| `archive_cas_merge` | `Moved` per source (under its folder) | one parent per source |
| `archive_cas_purge` | none: retires data under a signed consolidation | unchanged |
| Task edit, move, delete | `Changed`, `Moved`, `Deleted` | the next sync point |
| Task sync | records resolving each discrepancy, `Same` and `Distinct` | one, with each participant's previous point as a parent, signed by each |

## Questions for Jay

1. When origins are judged the same, is the item the set of origins, or
   does one origin become canonical (as `Merged` keeps one side per
   property)?
2. May an agent resolve a discrepancy under a stated policy, with Jay
   approving the consolidation, or does every `Same` and `Distinct` need
   Jay?
3. Should equal content with equal placement resolve to `Same` without
   asking, or always be a proposal?
4. Existing archives: treat each current manifest as a consolidation with
   no records (format v3 starts from them), or write records reconstructed
   from the sidecar logs?

## Order of work

1. **Records and consolidations in `data`**, BLAKE3 over canonical bytes,
   with the archive tools as the first caller: remove, add, exclude, unpack
   and merge write records into a consolidation instead of sidecar logs.
   This is lesson 2.
2. **Proposals for archives**: `archive_cas_reconcile` lists proposals and
   discrepancies between two archives or folders (equal content, name and
   size, normalised text, zip relations), and records `Same` or `Distinct`.
   `loadngo-cpp` and `loadngo-cpp.pre-mailmap-backup-20260820` in
   `pudding-20260917` are a first case.
3. **Directory nodes** (lesson 3), so subtree records and every
   consolidation stay small.
4. **Task**: canonical BLAKE3 hashing in `data::entity` and `data::sync`;
   sync emits records and consolidations; skeletal keys fold case and
   Unicode.
5. **Author and approver signatures** on consolidations.
