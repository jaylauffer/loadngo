# Repository execution, storage, and thermal plan

Status: implementation and remaining integration, 2026-11-05. Repository v2
import storage, `verify`, checkout CAS restores, and send payload reads use a
command-owned proactor and thermal admission. CDC ingest admits thermal policy
at least every 64 KiB of hashing and boundary detection, as well as before
source reads and CAS writes. Runtime fetch and CLI `receive` read framed input through
a separate bounded blocking-I/O worker and hash each object block on the CPU worker.
The direct non-runtime receive API retains synchronous stream reads. Receive
partial-file writes and sync run on a separate bounded blocking-I/O lane with thermal
admission before each bounded write and before sync, then CAS ingestion uses the runtime.
`init` and `commit` chunk changed files and write manifests, trees, and versions
through runtime CAS APIs. Runtime chunk boundary selection and whole-file hashing
process 64 KiB buffers on the bounded CPU worker, returning completed chunks to the
command thread for thermally admitted CAS writes. Tree diff, portions of metadata,
Git-pipe processing, and non-runtime API fallbacks remain synchronous. Status directory
traversal and ignore-file reads run on the bounded blocking-I/O lane after thermal
admission; regular-file hashes continue through thermally admitted proactor reads and
bounded CPU work. New head
signatures and runtime-path trusted-head verification run on the thermally admitted
bounded CPU worker, with secret
key bytes zeroizing while queued. Working-file hashes use
admitted proactor reads and dispatch each reusable bounded buffer through the
single bounded CPU worker before returning it for reuse. This document records
the evidence and remaining work while preserving repository history and CAS invariants.

## Current implementation evidence

The `pq-repository` base at `bc294c4f` introduced signed heads, version/tree/file
objects in Archive CAS, Git import, checkout, status, commit, verify, send/receive,
and repository-aware CAS collection. This isolated `repo-runtime` worktree adds
the v2 chunk-manifest format and runtime-backed command paths described below.
The base worktree's documented Loadngo-history import (465 commits, 79 seconds,
93 MB) is one Mac mini observation, not a general performance guarantee. See
[REPOSITORY.md](REPOSITORY.md).

The CLI remains synchronous at its public boundary, but runtime-aware operations
drive a command-owned platform proactor, a bounded CPU lane, and a lazily created
single-worker blocking-I/O lane for pipe reads/writes, filesystem traversal, checkout
publication, and receive partial-file writes/sync. File I/O uses
`IoPort` and owned buffers where supported; the thermal controller admits and
delays optional I/O and CPU work. `enqueue_work` only schedules completion
handling on the proactor and is not a worker executor. Remaining synchronous
steps include import traversal scheduling, per-range source-file metadata checks in
chunk reconstruction, filesystem preflight checks, some metadata/hash work, and
local object-existence checks in non-runtime push fallbacks. Runtime
send writes its framed output through the blocking-I/O lane;
runtime receive and fetch read owned pipes on that lane. Fetch's error path
terminates the sender to unblock its reader.

The synchronous Archive CAS restore adapter checks bounded worker capacity before
moving an `FnOnce` file operation into the queue. If completion wakeups still hold
all lane permits, it services the proactor until a completion releases capacity,
then submits the required operation. This keeps the worker pool's nonblocking
`Saturated` result from turning a long multi-file checkout into a spurious error.

`RepositoryRuntime` teardown shuts down and joins both bounded worker lanes
then dispatches their queued wakeup completions before stopping the proactor
and running its kernel-I/O shutdown drain. This ordering releases worker
capacity reservations and lets outstanding kernel I/O reach terminal
completion before the backend is released. Drop cannot return shutdown errors,
so backend drop remains the fallback cleanup path. Worker jobs are not
forcibly cancellable: shutdown drains queued work and waits for running
closures, so long-running or blocked worker operations still need
operation-specific cancellation before the CLI can promise prompt interruption.

At the 2026-11-05 checkpoint, branch heads and retained signature claims are read
with a 64 KiB per-file limit. Trusted-branch lookup skips missing, malformed, or
untrusted candidates while propagating file, thermal-admission, worker, and
proactor failures. GC parses one retained claim at a time and walks its ancestry
immediately, avoiding a second vector of full signatures or version roots. The
Mac run `cargo test --offline --locked -p data --test repository` passed 20 tests
with one opt-in full-history test ignored, including
`oversized_head_and_signature_claim_files_are_bounded`. Locked CLI check, strict
all-target/all-feature Clippy, formatting, and diff checks passed. The same 20 tests
passed on Dolores Linux ARM64, with `LOADNGO_CAS_MIN_FREE_GIB=0` and `TMPDIR` on the
home volume because `/tmp` is a 2 GiB tmpfs. Dolores also passed the workspace
all-features check and strict all-target/all-feature Clippy after the Linux
`gfx-metal` fallback fix. Formatting and diff checks pass. Target-hardware thermal
measurements remain open.

The v1 repository format is whole-file addressed: a tree file entry names one
`ArchiveObject`. Production v2 trees can name a canonical chunk manifest with
ordered immutable Archive CAS chunks and a whole-file digest. GC, transfer
closure/planning, checkout, verification, and fetch/receive traverse this
representation. New writes use `loadngo-buzhash64-cdc-v2`: a 64-byte rolling
Buzhash with a correctly rotated outgoing byte and 8/64/128 KiB
minimum/target/maximum. V1 manifests remain readable. The profile is inspired
by Xet's CDC approach and is not Xet wire-compatible.

[`repository_chunk_probe`](../data/src/bin/repository_chunk_probe.rs) exercises
this production chunking profile against whole-file CAS, verifies stored chunks
and canonical manifests, and reconstructs both forms. A one-byte insertion sweep
on `data/src/archive_cas.rs` reused 42,497–184,170 bytes across five chosen
offsets. A separate probe run on the committed 112,492-byte `Cargo.lock` produced
two chunks and a 395-byte manifest; whole-file and CDC reconstructions had the
same BLAKE3 identity. That validates this fixture and exercises the actual Rust
chunker, not repository-wide economics.

The tracked-working-tree driver was then run on this `repo-runtime` checkout:

```sh
cargo build --release --offline --locked -p data --bin repository_chunk_probe
python3 scripts/repository_chunk_workload.py \
  --repo-root . \
  --probe "$PWD/target/release/repository_chunk_probe" \
  --cas-root /path/on/a/large-volume/repository-chunk-workload
```

The captured TSV is [`REPOSITORY_CHUNK_WORKLOAD_2026-11-05.tsv`](REPOSITORY_CHUNK_WORKLOAD_2026-11-05.tsv).
It covered all 787 tracked regular files (30,001,737 logical bytes), including
Rust, fonts, models, safetensors, and PDFs. The v3 policy selected whole-file CAS
for 421 files below 8 KiB and CDC manifests for the remaining 366. It stored 1,489
objects with 30,118,983 file bytes and 33,943,552 allocated bytes. Pure CDC stored
1,902 objects with 30,239,311 file bytes and 35,635,200 allocated bytes; whole-file
CAS stored 779 objects with 30,001,649 file bytes and 31,780,864 allocated bytes.
The v3 hybrid saved 120,328 file bytes and 1,691,648 allocated bytes against
all-files CDC. Compared with whole-file CAS, it used 117,334 more file bytes (0.39%)
and 2,162,688 more allocated bytes (6.81%). The three stores were independent, so
their deduplication results do not contaminate each other. Allocated bytes are the
sum of each object's `st_blocks * 512` on this APFS volume, not a portable measure
of physical-device writes.

Across all 787 files, summed probe-stage times were 28.796 s for chunk ingest,
22.538 s for v3 hybrid ingest, 11.543 s for whole-file ingest, 4.765 s for chunk
reconstruction, and 11.790 s for whole-file restore plus hash verification. The run
took 101.241 s wall time and 26.944 s child CPU, with 8,896 KiB peak child RSS. All
787 per-file final thermal samples were Nominal. Each file used a fresh probe
process, so wall and stage timings include process/proactor startup; this remains
local benchmark evidence, not target-hardware thermal evidence or changed-file
transfer measurement.

The local CLI delta benchmark at [`REPOSITORY_DELTA_BENCHMARK_2026-11-05.tsv`](REPOSITORY_DELTA_BENCHMARK_2026-11-05.tsv)
uses two Git commits containing a deterministic 2 MiB binary asset, with the second
commit inserting byte `ff` at its start. The benchmark script creates an ML-DSA-65
key, imports and signs the baseline, sends it to an independent local receiver,
imports the updated commit, sends the delta, records a framed stream, verifies the
receiver, and byte-compares its checkout. Run it with release binaries and a new
output path outside a Git checkout:

```sh
cargo build --release --offline --locked -p data --bin loadngo-repo
cargo build --release --offline --locked -p loadngo-pq-auth --bin loadngo_pq_auth
python3 scripts/repository_delta_benchmark.py \
  --repo-cli "$PWD/target/release/loadngo-repo" \
  --keygen-cli "$PWD/target/release/loadngo_pq_auth" \
  --work-root /path/on/a/large-volume/pq-delta-benchmark
```

The 2,097,153-byte changed file sent as five new objects totaling 80,386 payload
bytes (3.833% of the changed file); the complete framed stream was 91,468 bytes
(4.362%). Receiver verification covered two versions and checkout byte comparison
covered both files. One local Mac run measured 0.934 s baseline import, 0.214 s
incremental import, 1.162 s initial send, 0.135 s incremental send, and 0.050 s
checkout. This demonstrates strong reuse for this prefix insertion; it is a
synthetic local-store case and does not measure SSH/network transfer or broader
change patterns.

For a full tracked working-tree run, build `repository_chunk_probe` and invoke
`scripts/repository_chunk_workload.py --repo-root <checkout> --probe <absolute-probe-path>
--cas-root <new-empty-path-outside-checkout>`. It scans `git ls-files`, skips and
counts missing paths, symlinks, and non-regular files, and runs each regular file
through the production probe. Pure chunked, whole-file, and v3 hybrid objects go to
three separate shared CAS roots, preventing one strategy's writes from inflating
another's reuse. The TSV report groups logical bytes, chunk references and new
objects, canonical manifest sizes, hybrid whole/chunked file counts, newly stored
file bytes by extension, total allocated CAS bytes, ingest/restore latency, CPU time,
and peak child RSS. Per-file probe processes mean the CPU and RSS totals include process
startup; thermal pressure is reported per run. This workload does not measure
repository CLI import, changed-file wire transfer, or idle/active OS thermal
telemetry. The CAS roots must be new or empty and are retained for inspection.

An earlier local smoke run used a deterministic 1 MiB random input: 17 chunks;
the identical second ingest reused all 17; inserting one byte at offset
300,000 reused 16 of 17 chunk objects. Whole-file CAS stored the first input,
reused it on the repeat, and stored a new object after insertion. Its hash
matched each CDC manifest hash, and both whole-file and chunked reconstructions
matched their input bytes. Elapsed times were 397 ms, 184 ms, and 203 ms
respectively; these include whole-file verification and both reconstructions,
and are only one local sample. Source reads, bounded whole-file CAS ingest,
raw CAS payload writes, raw-object verification, range reads, and raw-object
restore use the platform proactor. Compressed-object verification, range reads,
and restore retain synchronous decompression. The shared thermal controller
reported Nominal once per run; no pressure transition was exercised. This
demonstrates resynchronization for that fixture, not representative
deduplication or performance. The probe's chunking/hash, file sync, optional
compression, and atomic publication remain synchronous on its caller. In
production, working-file hashes, runtime head signing/verification and receive
payload hash updates use the bounded worker where noted below. Windows proactor
handles are opened overlapped. At the 2026-11-05 checkpoint, the full repository
CLI passed `cargo check --offline --locked --target aarch64-apple-ios -p data --bin
loadngo-repo`. The Mac cross-check
(`cargo check --offline --locked --target aarch64-unknown-linux-gnu -p data --bin
loadngo-repo`) stopped in native C build scripts because the cross compiler is
unavailable, so native Linux ARM64 validation was run on Dolores instead. The
standalone probe does not publish signed repository history.

A local insertion-position sweep used the 194,705-byte `data/src/archive_cas.rs`;
the base formed four chunks with a 596-byte manifest. One-byte insertions at
offsets 8,192, 20,000, 43,000, 100,000 and 180,000 reused 184,170, 162,743,
42,497, 63,634 and 173,569 bytes, respectively (mean 64.4% across these five
chosen offsets). All reconstructions matched their source, elapsed times were
98–112 ms, and each run observed Nominal thermal pressure once. The large
position-to-position variation is evidence against choosing a profile from a
single insertion example; this one source file still does not represent the
full repository history or binary assets.

The full unique-blob corpus reachable from `HEAD` (`bc294c4f`, 472 first-parent
commits) contains 2,274 Git blobs totaling 80,463,224 logical bytes. Applying
the persisted CDC boundaries yielded 3,023 chunk references, 2,787 distinct
chunk contents totaling 72,724,213 bytes, and 741,101 canonical manifest bytes.
That is 9.62% less unique payload than whole-file CAS, or 8.70% less after
manifest bytes. The corresponding object counts are 2,274 whole-file objects
versus 5,061 chunk-plus-manifest objects (2.23 times as many). The analysis used
SHA-256 chunk fingerprints and a direct port of the persisted boundaries; a
production Rust probe on `HEAD:Cargo.lock` independently matched its two chunk
boundaries and 395-byte manifest, and verified both restorations. The counts are
logical: compression, filesystem allocation overhead, read latency, and transfer
time were not measured. Separately stored model or LFS payloads were not included.

A production Rust probe against six files from canonical `github/dev` added direct
measurements for code, Cargo metadata, tiny model fixtures, and font assets. Each
baseline file was ingested into one shared CAS; then a one-byte prefix copy was
ingested into the same CAS. Every reconstruction matched its BLAKE3 identity.

| File | Baseline bytes | Chunks | Manifest bytes | Prefix-copy chunk bytes reused | Probe ms (base / edited) |
|---|---:|---:|---:|---:|---:|
| `Cargo.lock` | 112,492 | 2 | 395 | 41,689 (37.1%) | 95 / 86 |
| `data/src/archive_cas.rs` | 110,251 | 2 | 395 | 70,472 (63.9%) | 92 / 88 |
| `gpt-oss/tests/fixtures/tiny/model.gguf` | 598,240 | 10 | 1,197 | 541,223 (90.5%) | 262 / 158 |
| `decider/tests/fixtures/tiny/model.safetensors` | 1,019,208 | 14 | 1,599 | 897,669 (88.1%) | 371 / 266 |
| `text-shaping/fonts/NotoSansSC-GB2312-Regular.otf` | 2,004,496 | 37 | 3,900 | 1,955,003 (97.5%) | 899 / 381 |
| `assets/fonts/hussar-print-font/HussarPrintMc-vq6A.otf` | 2,995,568 | 43 | 4,508 | 2,880,567 (96.2%) | 1,150 / 518 |

Across the six prefix copies, 6,386,623 of 6,840,261 changed-file bytes reused
existing chunks. The edited chunks plus manifests added 465,632 bytes versus
6,840,261 bytes of whole-file CAS payload, a 93.2% logical reduction for this
one-byte-prefix workload. All 12 probe runs observed Nominal thermal pressure;
the elapsed figures include each run's verification and both reconstructions.
This is a selected-file workload, not a whole-repository performance result.

There are 911 blobs smaller than the 8 KiB minimum (3,069,305 bytes total).
Their canonical manifests account for 265,453 bytes. Since each non-empty such
blob becomes one chunk plus one manifest, a future deterministic raw-file rule
for that size range would project 910 fewer objects and remove those manifest
bytes in this corpus; it would not reduce logical payload bytes. This is a
measured candidate for a versioned storage policy, not a change to the existing
profile or repository format.

## Runtime architecture and remaining work

Runtime-aware repository commands own a platform `Proactor`, thermal admission,
a bounded CPU worker lane, and a lazily created bounded blocking-I/O lane for stream
reads and receive partial-file writes/sync.
Public commands synchronously drive those
resources to completion; they do not yet expose an asynchronous operation API.

1. The runtime dispatches proactor completions while waiting for I/O and CPU
   results. The CLI remains synchronous at its public boundary; it does not
   expose an asynchronous operation API yet.
2. File descriptor reads and writes use `IoPort` with owned `IoBuf` buffers,
   explicit offsets, and completion-driven state changes where supported.
   Some traversal scheduling and filesystem operations that `IoPort` cannot
   express still use synchronous OS APIs; runtime status walks, checkout publication,
   and branch-head publication use bounded blocking-I/O or worker lanes. Selected CPU hashing, signature
   checks, and metadata/tree work use the bounded worker lane; completion
   callbacks only publish results and schedule the next state transition. The
   host-desktop `offload` pool is not a direct fit for the standalone repository
   CLI: it is tied to a GUI host's completion path and accepts queued work
   without a queue bound. The
   data crate now exposes `archive_cas::worker::ArchiveCasWorkerPool`: fixed
   workers, a bounded queue, and a total outstanding limit that includes results
   not yet taken and wakeups not yet dispatched. Results post a completion to the
   supplied proactor. Runtime paths use it for metadata serialization, new-head
   signing and verification, working-file hashes, tree comparisons, and incoming
   receive-payload hashes. Blocking stream reads and receive partial-file writes
   and sync use a separate bounded lane while the proactor continues dispatching;
   the object buffer is returned for hashing only after its write completes.
   Runtime CDC boundary/hash work uses the bounded CPU worker in 64 KiB steps, with
   thermal admission before every step; other hashes retain their documented paths.
   Runtime receive verifies existing duplicates with
   bounded CAS proactor range reads and hashes each returned buffer on the bounded
   CPU worker, reusing the buffer between reads.
  Trusted-branch lookup skips malformed or untrusted candidate heads, while
  filesystem, thermal-admission, worker, and proactor-completion failures
  propagate instead of being treated as untrusted heads.
   Callers apply thermal admission before
   submission and consume or drop each result to release capacity.
3. In-flight worker jobs and I/O buffers have fixed limits. Current command
   paths generally await one worker result at a time, bounding memory and
   providing backpressure. In-flight repository cancellation/checkpoint APIs
   are not implemented. Runtime teardown stops both worker lanes, dispatches
   their pending wakeups, then stops the proactor and drains kernel I/O. Joining
   a worker can still wait indefinitely for a blocked closure; prompt
   cancellation needs operation-specific support.
4. The thermal governor samples through the platform provider on a coarse
   cadence. Fair pressure reduces optional work, Serious minimizes it, and
   Critical pauses it until recovery. CAS I/O, file hashing, and worker
   submission apply admission at bounded steps. Probe samples so far were
   Nominal only; Fair/Serious/Critical transitions and target-hardware thermal
   safety remain unverified.

Thermal feedback changes scheduling only. It never changes hashes, signatures,
chunk boundaries, traversal order in canonical manifests, or operation
outcomes. Remaining work includes moving more CDC and verification hashing onto
workers, offloading traversal and remote preflight/process lifecycle where that
improves bounded responsiveness, target-specific builds, and representative
real-hardware idle/active measurements.

Do not wrap the entire synchronous command in `enqueue_work` and call that
proactor integration: that only moves a blocking command into the dispatch
thread. `ArchiveCasStorage::ingest_file_with_proactor_admitted` streams source
reads and temporary-object writes in bounded buffers, with a caller admission
hook before each read and write. If the object already exists, or another
writer publishes it concurrently, the admitted verifier checks it through
bounded proactor reads before ingest returns. `add_content_with_proactor` writes
bounded payload buffers, and its `_admitted` variant invokes the hook before
each payload write and during existing-object verification. For payloads at or
below 256 KiB it transfers the owned `Vec` directly into `IoBuf`, avoiding a
second full payload copy; larger objects are written in bounded slices;
`read_range_with_proactor` reads bounded raw ranges, and its `_admitted` variant
gates every raw read. `read_range_into_with_proactor_admitted` fills caller-owned
storage so reconstruction can reuse one buffer across chunks;
`read_object_with_proactor_admitted` reads and hash-checks a complete metadata
object under an explicit in-memory size ceiling, providing the bounded path for
tree and version decoding. `verify_object_with_proactor` hashes raw objects as it
reads them. These methods drive the caller-owned proactor until each operation
completes.
`ingest_chunked_file_with_proactor_admitted` applies the reusable versioned CDC
profile directly to a regular source file: it reads through a reusable proactor
buffer, hashes the whole file, stores each chunk through admitted CAS writes,
checks the source stamp did not change, enforces a caller-supplied chunk limit,
and returns a validated manifest. Its bounded working set is one read buffer,
one maximum-sized chunk, and the manifest references. Git import now copies each
`cat-file` blob to a scratch file with a fixed 1 MiB buffer, checks available disk
space from the declared blob length, then calls this admitted chunking API. Each
`cat-file` startup, each request/response, and the bounded pipe-to-scratch copy
plus file sync run on the blocking-I/O worker; the full blob is never held in
memory.
`verify_object_with_proactor_admitted` and
`stream_object_to_with_proactor_admitted` call a caller-supplied admission hook
before object reads, so a command can apply its thermal policy during
verification and send. Compressed verification and range reads use
proactor-backed source reads with synchronous zstd decoding; admission runs
between bounded decoded-output buffers. Hashing and decoding still run on the
caller.
Compressed restore drives the decoder synchronously over proactor-backed source
reads and destination writes, with thermal admission between bounded steps. Runtime
checkout routes compressed source-file open and close through the thermally admitted
blocking-I/O lane.
`stream_object_to_with_proactor` verifies an object before emitting it, then
streams raw CAS bytes to a caller-provided writer through bounded proactor
buffers while hashing again. The writer remains synchronous and may have
received a partial object if it or the second-pass check fails, so a framed
sender must not write its completion marker until the call succeeds. Compressed
objects use proactor-backed source reads and bounded synchronous decoder reads,
with admission before each decoded read and writer call.
`restore_object_to_path_with_proactor` also streams raw objects through bounded
reads and writes, verifies the digest, and publishes with a no-replace hard
link. Its `_admitted` variant calls the admission hook before each raw read and
write. Compressed restore uses proactor-backed source reads and destination
writes; zstd decoding remains synchronous with admission between bounded steps.
For admitted in-memory add and regular-file ingest, optional compression now
admits between bounded reads, encoder writes, and decompressed verification
reads. Compression, hashing, file sync, and publication still run on the caller.
The probe exercises these CAS paths. File ingest and raw restore transfer each completed
read buffer directly into the paired write, without cloning the payload. Import,
`verify`, checkout, send planning/payload streaming, receive CAS ingestion, and receive
closure/ancestry checks call runtime APIs; local stdin frame handling and the standalone
probe's chunk hashing remain synchronous. Production import/init/commit run CDC boundary
and whole-file hashing on the bounded CPU lane. Chunk checkout reads and writes directly between bounded proactor buffers,
without per-chunk temporary files. Do not add an unbounded thread-per-file model
or a polling loop.

## Xet-inspired storage evolution

Use [Xet's chunking design](https://huggingface.co/docs/xet/chunking) and
[file reconstruction model](https://huggingface.co/docs/xet/file-reconstruction)
as references for content-defined boundaries and sparse transfer, not as a
dependency on Xet's service or complete wire protocol. Keep immutable content
identity and repository lineage separate:

- The current versioned `loadngo-buzhash64-cdc-v2` profile fixes a 64-byte
  rolling window and 8/64/128 KiB minimum/target/maximum sizes, inspired by
  Xet's published settings. V1 manifests remain readable. A selective source-history sample now establishes
  baseline reuse and manifest costs; broader history, binary assets, and
  per-file-type analysis remain open. Any changed parameters require a new
  profile identifier.
- Production v2/v3 trees hold ordered `(chunk hash, length)` references through
  canonical manifests, with total length and whole-file digest. Chunks are
  immutable Archive CAS objects; v1 whole-file entries remain readable.
- Reconstruct by streaming verified chunks into a temporary file, verify each
  chunk and the final whole-file digest, sync it, and only then publish the
  checkout entry. A missing chunk or mismatch leaves the previous destination
  untouched.
- Reachability, GC, purge planning, send/receive, repair, verify, checkout, and
  fetch already traverse manifests and referenced chunks. Continue measuring
  transfer and reconstruction costs on representative repositories; never
  publish a signed head before its complete reachable closure is durable.
- Store chunks as ordinary CAS objects for now. The complete source-history
  sample found 2.23 times as many chunk-plus-manifest objects as whole-file
  objects. A first layout experiment repacked all 1,489 objects from the
  measured v3 hybrid working-tree CAS into one data file plus a sorted index
  (48 bytes per object): allocated storage fell from 33,943,552 to 30,195,712
  bytes, saving 3,747,840 bytes (11.04%). Full sequential byte comparison and
  256 deterministic sampled object reads matched. Its warm-cache timing is
  exploratory: loose reads opened each file while packed reads reused one open
  pack handle, and it did not model CAS hashing, partial ranges, crashes, or
  concurrent readers. This storage result justifies a production pack prototype
  with independently verifiable chunk identities, crash-safe publication,
  random-range reads, repair, and live-object repacking for GC; it does not yet
  justify replacing loose-object storage. Reproduce with
  `python3 scripts/repository_pack_layout_benchmark.py --cas-root
  /path/to/hybrid-v3 --output /tmp/cas-pack-layout`. Measurements are in
  `REPOSITORY_PACK_LAYOUT_2026-11-05.tsv`.
  A native Rust prototype now writes the same 1,489-object snapshot through
  the caller-owned proactor, applies `RepositoryRuntime` thermal admission at
  each bounded read/write and index batch, routes source/pack file lifecycle
  through the bounded blocking-I/O lane, publishes data before the checksummed
  index, and rechecks every object's BLAKE3 hash through bounded proactor reads.
  The resulting index is 71,528 bytes; measured allocated bytes are unchanged at
  30,195,712 for the snapshot and index, preserving the 11.04% saving. The
  same run then read 249 deterministic ranges (up to 4 KiB each) through one
  open pack handle and matched each against its loose object. Partial ranges
  alone do not prove object integrity; callers must validate a complete object
  or reconstructed file. The source CAS remains untouched. This prototype is
  standalone: normal CAS
  lookup, send/receive, compressed `.zst` objects, repair, and GC do not use it.
  Run `cargo run -p data --bin archive_cas_pack -- --cas-root
  /path/to/cas --destination /path/to/new-pack-directory`; details are in
  `REPOSITORY_PACK_PROTOTYPE_2026-11-05.tsv`.
- Runtime-aware import/init/commit write new regular files below the 8 KiB CDC
  minimum as whole-file CAS entries, including empty files; larger files use
  chunk manifests. These trees use v3 to identify the selection policy. Unchanged
  files already represented by earlier chunk manifests retain those references.
  The source-history projection is 910 fewer objects and 265,453 fewer manifest
  bytes, with unchanged logical payload. Validate on binary assets and
  representative stores before treating this projection as general.

The existing BLAKE3 object names and signed version ancestry can remain the
identity layer for v1 history. A v2 tree must identify its chunk profile and
file-manifest format so independent machines reconstruct identical bytes.
Cross-repository deduplication must also have an explicit privacy policy; shared
chunk identity can reveal that content is already present.

## Implementation sequence and evidence gates

### Current production call-site map

The initial repository implementation came from the `pq-repository` base at
`bc294c4f`; this isolated runtime worktree now contains the follow-on integration:

| Current call site | Current behavior | CAS/runtime integration point |
|---|---|---|
| `data/src/repository.rs::Repository::head_with_runtime`, `branches_with_runtime`, and `heads_with_runtime`; CLI `heads` | Reads and validates ordinary branch-head JSON (capped at 64 KiB per file) and lists branch filenames on the thermally admitted bounded blocking-I/O lane; trusted-head and runtime trusted-branch lookup plus receive/import/init/fetch paths use these APIs. `heads` enumerates the directory and reads every head in one bounded blocking-I/O task. Historical signature claims used by GC have the same per-file cap; GC parses one claim and walks its history at a time, avoiding a retained vector of full signature objects or root hashes. An oversized claim aborts collection. | Route remaining synchronous commands through runtime APIs when those commands own a runtime. |
| `data/src/repository/import.rs::Importer::blob` and `Importer::commit` | Copies Git blob bytes through a reusable 1 MiB buffer into a scratch file, checks declared size, then stores small files whole or processes 64 KiB CDC/hash blocks on the bounded worker before admitted proactor CAS writes; rev-list output filtering and Git commit/tree parsing run on the bounded CPU worker with their input buffers returned for reuse, metadata at least 32 KiB decode/canonicalize on that lane, all existing-head version and ancestry reads use runtime APIs, parent-version/tree-diff reads use runtime APIs, tree comparisons run in batches of 32 nodes, and new-head signing runs on the thermally admitted worker lane. Initial Git ref resolution, merge-base checks, commit-list enumeration, cat-file startup/exchanges, blob spooling, and scratch sync run on the thermally admitted blocking-I/O lane. | Move tree traversal scheduling off the caller; keep blob data streaming bounded while evaluating direct CDC ingestion from the persistent Git pipe. |
| `data/src/repository.rs::Repository::verify_with_runtime` and CLI `verify` | Verifies the trusted head on the bounded worker lane, reads bounded version/tree/manifest objects through the proactor, decodes/canonicalizes metadata objects at least 32 KiB on that worker, and verifies physical objects through thermally admitted CAS range reads with each 256 KiB hash update on the bounded CPU worker. Tree closure uses an explicit DFS stack, validates child totals, and rejects unresolved/cyclic child references without recursive call-stack growth. Graph scheduling and some metadata hashing remain synchronous. | Route remaining metadata reads and CPU-heavy validation through bounded runtime work where useful. |
| `data/src/repository/transfer.rs::plan_send_with_runtime`, `write_stream_with_runtime`, and `fetch_with_runtime` | Push runs remote process startup and completion on the blocking-I/O lane; parses the `heads` listing on the bounded CPU worker, checks hinted object existence on the blocking-I/O lane, validates versions through runtime reads, and checks ancestry through runtime APIs. The stream feed stays on the command thread to drive proactor reads, while its owned writer is transferred to the bounded blocking-I/O lane. Each object is hash-checked before the `E` terminator is sent. Fetch consumes the remote pipe on a bounded worker and kills/waits for the sender on error before closure verification and head advancement. If the local closure is damaged, fetch sends full history for repair. Non-runtime fallback checks and process lifecycle remain synchronous. | Reuse complete content across unrelated local branches where safe; move the stream feed/controller off the command thread only if proactor ownership and bounded backpressure are retained. A failed stream never writes the `E` terminator. |
| `data/src/repository/transfer.rs::fetch_with_runtime`, `receive_object`, and `check_closure_with_runtime` | Fetch and the `receive` command read framed input through a separate bounded blocking-I/O worker using owned readers. Object buffers are written to the partial file and then hashed on separate bounded lanes, with ownership returned before reuse. Partial-file sync also runs on the blocking-I/O lane after thermal admission. Existing duplicates and every object in runtime closure checks use thermally admitted bounded CAS range reads; each returned buffer is hashed on the bounded CPU worker and reused. Fetch's error path kills and waits for its sender, which unblocks the pipe reader. The proven file is ingested and the full head closure/ancestry is checked before publication. Incoming head signature verification runs on the bounded worker lane; runtime head publication uses one admitted worker operation for lock, compare-and-swap, synced signature claim, and synced head replacement. | Preserve bounded-memory verification and damaged-copy rollback; extend the same worker/proactor pipeline to remaining synchronous hashes. |
| `data/src/repository/work_tree.rs::status_with_runtime`, `commit_with_runtime`, and `init_with_runtime` | Status directory traversal and ignore-file reads run as one thermally admitted task on the bounded blocking-I/O lane, returning paths for hashing on the command thread. Working files are hashed through thermal-admitted proactor reads; each bounded read buffer and BLAKE3 state are processed on the single bounded worker lane and the buffer is reused across reads and files. New regular files below 8 KiB remain whole-file CAS objects; larger changed files send 64 KiB CDC/hash blocks through the bounded CPU lane, return the buffer, and publish completed chunks through thermally admitted proactor CAS writes. Canonical manifests/trees/versions serialize on the bounded worker lane. Metadata objects at least 32 KiB decode/canonicalize on workers; parent/tree reads and tree comparison/move detection use runtime/proactor paths. New head signatures and runtime head publication run on thermally admitted workers; import traversal scheduling and small-object parsing remain synchronous. Head publication holds the branch lock across compare-and-swap and synced signature/head writes. A status-to-ingest content mismatch aborts the commit. | Bound traversal memory for very wide directories. |
| `data/src/repository/work_tree.rs::checkout_with_runtime` | Verifies a branch head on the bounded worker lane; reads trees/manifests with bounded runtime reads. Raw objects and chunk manifests restore through bounded proactor reads and writes; chunks are checked individually and as a whole before the reconstructed file is published into the temporary checkout. CAS source/output file opens and closes, source metadata checks, destination preflight/sync, and no-replace publication run on the thermally admitted bounded blocking-I/O lane. Restore waits for completion wakeups to release worker capacity before submitting the next required file operation. Directory creation, symlink creation, mode updates, the synced checkout marker, final directory rename, and failed-checkout cleanup use that lane too. | Validate failure cleanup/mode behavior with the repository integration suite. |
| `data/src/repository/work_tree.rs::merge_with_runtime` | Verifies both trusted heads on the bounded worker lane, reads both histories and their base trees through the runtime, and runs bounded non-overlapping line diffs for small UTF-8 files on the thermally admitted bounded CPU worker. Merged files above the CDC minimum are chunked and stored through admitted proactor CAS writes. It writes merged tree/version through runtime CAS and bounded serialization work, then verifies the complete new closure before publication. New head signing and publication run on the thermally admitted bounded worker lanes; the publication worker holds the branch lock across compare-and-swap, synced signature claim, and synced head replacement. A conflict or failed write leaves the branch head unchanged. | Add recursive handling for multiple best bases, conflict-marker workflows, and broader merge limits; profile large history/tree CPU costs. |
| `data/src/bin/loadngo-repo.rs::log` and `Repository::first_parent_history_with_runtime` | Verifies branch heads and walks/reads every first-parent version through the command runtime. | Measure history traversal and bound memory for very long histories. |
| `data/src/bin/loadngo-repo.rs::diff` and `Repository::changes_between_with_runtime` | Verifies both branch heads and loads uncached version/tree objects through the proactor; objects at least 32 KiB decode/canonicalize on bounded workers, and batches of up to 32 tree pairs are compared on the worker lane. Emits path-level changes and moves without reading file payloads. Traversal scheduling remains synchronous. | Measure against deep and highly branched repositories; bound traversal memory for extremely wide changed trees. |

Runtime-aware import, init, commit, status, checkout, verify, log, diff, merge,
send, fetch, and receive paths now use the runtime for their principal object
flows. Synchronous API fallbacks, some trusted-branch candidate orchestration,
Git pipe traversal scheduling, and non-restore range metadata checks remain outside the
worker/proactor path. Checkout CAS source/output file opens and closes, source metadata
checks, destination preflight/sync, no-replace publication, and runtime branch-head
publication use admitted bounded blocking-I/O/worker operations. Runtime CDC,
duplicate-copy checks, and closure object verification use bounded worker hashing
with thermally admitted CAS/proactor reads.

1. **Runtime completeness:** add cancellation/checkpoint semantics and continue
   moving CPU-heavy CDC/verification work, traversal, and pipe handling into
   bounded runtime paths without changing object identities. Validate durable
   publication and shutdown on each supported host.
2. **Thermal evidence:** a new event-driven sampler records the Loadngo
   thermal provider and governor through deferred proactor deadlines. The
   2026-11-05 report contains a 20-second sampler-only macOS window and a
   12-second active 16 MiB delta run; all samples were NativePressure,
   Nominal/Baseline. The API returned no temperature or throttling data.
   These observations do not establish whole-system idle, sustained thermal
   safety, or wakeup counts. Still required: verified whole-system idle and
   sustained full-tree sampling, Linux hardware telemetry, thread counts,
   actual wakeup counts, and Fair/Serious/Critical transitions. The full-tree
   101-second workload only retained per-file/final Nominal observations.
   Reproduce with `cargo build --release -p data --bin
   repository_thermal_benchmark`, then run
   `target/release/repository_thermal_benchmark --duration-seconds 20`; for
   active sampling, run it with `--interval-seconds 2 -- <command>` around the
   delta benchmark (see `scripts/repository_delta_benchmark.py`). In the
   sampler-only window, process CPU was 0.001920 s user / 0.001551 s system,
   peak RSS 5,984 KiB, and 0 voluntary / 5 involuntary context switches. The
   active 16 MiB prefix-edit benchmark ran 12.017 s; it sent 5 delta objects
   with 102,341 payload bytes (0.610% of the changed file), and its framed
   stream was 113,423 bytes (0.676%). Child CPU was 1.443929 s user / 2.118710
   s system, peak RSS 21,648 KiB, with 7,783 voluntary / 29,106 involuntary
   context switches. Context switches are not wakeup counts. `pmset -g therm`
   failed with macOS error `0xe00002bc`; `/usr/bin/time -l` could not query
   `kern.clockrate` in this environment, so process resource usage came from
   Python `resource.getrusage`.
3. **Xet profile economics:** run the tracked-working-tree driver on additional
   representative code and binary-asset repositories; compare its results with
   repository CLI import/checkout and additional changed-file patterns. The v3
   small-file hybrid policy has one measured working-tree result. The end-to-end
   ML-DSA CLI delta matrix now measures four one-byte edit shapes on the same
   deterministic 16 MiB asset: prefix insertion transferred 102,341 payload
   bytes (0.610%); middle insertion 46,309 (0.276%); append 40,653 (0.242%);
   and middle overwrite 46,308 (0.276%). All four sent five objects, verified
   two versions, and byte-compared two checked-out files. These local synthetic
   results show this CDC profile resynchronizes better when the edit is away from
   the prefix; they do not represent larger files, real assets, repeated edits,
   or SSH/network cost. See `REPOSITORY_DELTA_EDIT_MATRIX_2026-11-05.tsv`.
   Run `scripts/repository_delta_benchmark.py --edit-mode MODE` with one of
   `prefix-insert`, `middle-insert`, `append`, or `overwrite` to reproduce.
   SSH/network transfer remains open. Any chunk-boundary changes require a new
   profile identifier.
4. **Post-quantum interoperability:** ML-DSA-65 is now registered as scheme ID `0x03`
   through pinned `aws-lc-rs = 1.18.1`; `loadngo-pq-auth keygen` defaults to this
   standardized scheme. Legacy Dilithium2 and Falcon512 remain registered. The same
   prefixed key/signature serialization carries the distinct ID and standardized raw
   key/signature encodings. Host compilation, strict Clippy, iOS, and Android target
   checks pass; Android used the installed NDK 30 clang toolchain. Native Linux ARM64
   validation on Dolores (Raspberry Pi 5, Rust 1.99) passed `cargo check --locked
   -p data --all-targets --all-features`, strict data Clippy, the workspace all-features
   check, and workspace strict Clippy. The first workspace check exposed unguarded
   Apple-only text rasterization in `gfx-metal`; a Linux error fallback and scoped
   imports fixed it without changing the Apple implementation. The repository
   integration suite then passed 20 tests on Linux, with the opt-in full-history audit
   ignored. That run used `LOADNGO_CAS_MIN_FREE_GIB=0` and `TMPDIR` on the home volume
   because Dolores' `/tmp` is a 2 GiB tmpfs. The x86_64 GNU cross-check on this Mac
   remains unavailable because `x86_64-linux-gnu-gcc` is missing. The deterministic
   ML-DSA-65 key-generation
   result matches NIST ACVP FIPS204 test case 26 byte-for-byte for both raw public and
   expanded private key encodings; source blob SHAs are recorded in
   `pq-crypto/tests/vectors/mldsa65_keygen_acvp.json`. The opt-in bidirectional signature
   interop check passes with OpenSSL 3.6.5 for the repository's empty-context pure-message
   interface (OpenSSL verifies the loadngo signature and loadngo verifies the OpenSSL
   signature). Context-bearing ML-DSA signature interoperability vectors remain
   unverified. On 2026-11-05, a local CLI smoke generated an ML-DSA-65 keypair,
   initialized and signed a repository, sent it to a local store, fetched it into a
   third store, verified both received closures, and checked out byte-identical files.
   The regression test `runtime_cli_mldsa_send_fetch_verify_and_checkout` covers that
   runtime CLI path. `git::runtime_cli_imports_and_resumes_git_history_with_mldsa`
   imports a four-commit Git history through the CLI, resumes it from an older ref,
   confirms both stores have identical versions and commit order, verifies both
   closures, and checks out the result with its symlink and executable bit. The
   repository integration suite passes 20 tests on macOS and Linux ARM64, with one
   opt-in full-history test ignored, including the ML-DSA chunked-file round trip,
   runtime CLI transfer, and a 2,048-level tree verification case.

   On 2026-11-05, the runtime CLI imported canonical `github/dev` at
   `bc294c4f` (472 commits) into a temporary store in 136.13 s (22.62 s user,
   16.30 s system). Runtime `verify` checked all 472 versions, 2,092 trees, and 2,274
   files (80,463,224 bytes); the CLI log's commit IDs matched Git's complete
   first-parent order. A fresh run checked out the signed head: 787 files and
   29,723,108 bytes. Comparing against `git archive` after excluding the expected
   root `.loadngo-repo` checkout marker found 972 matching paths, bytes, executable
   bits, and symlink targets. The opt-in test
   `git::full_history_snapshots_match_git_without_rewriting_every_worktree` then
   imported and audited all 472 versions: each Git snapshot's path/mode map and every
   unique blob's BLAKE3 identity matched the verified CAS trees (2,274 distinct Git
   blobs). Run it with `LOADNGO_REPOSITORY_HISTORY_GIT_DIR` and
   `LOADNGO_REPOSITORY_HISTORY_GIT_REF` set to the source checkout and ref. This audits
   every historical snapshot without materializing 472 full working trees. The same
   opt-in test now also builds one trusted-history capability and physically checks out
   every version through the transactional runtime checkout API, deleting each output
   before proceeding. It compares each file's bytes/hash and executable bit, each
   symlink target, and each checkout's file count against Git.
   Re-running that opt-in audit on the current `repo-runtime` worktree against
   `refs/remotes/github/dev` passed in 163.07 s, again matching all 472 snapshots
   and 2,274 unique blobs. The full per-version physical checkout run then passed all
   472 versions and 2,274 unique blobs in 5,023.37 s; it reported
   `matched 472 historical Git snapshots and 2274 unique Git blobs against verified CAS trees`.
   It compared file bytes/hashes, symlink targets, executable bits, and file counts for
   every materialized tree while retaining at most one checkout at a time. These runs
   did not capture OS thermal telemetry; their temporary stores were removed. AWS-LC
   makes the stable
   ML-DSA APIs available in `aws_lc_rs::signature`; this validates API availability, not
   a FIPS-validated deployment configuration.
5. **Packed-container integration:** the standalone proactor-written pack
   prototype proves index parsing, per-object BLAKE3 verification, and
   data-before-index publication on macOS while retaining the loose source.
   Production adoption remains gated on normal CAS lookup and bounded
   random-range reads, crash recovery (including Windows directory durability),
   compressed objects, repair, transfer, and GC that can repack only reachable
   objects. Preserve independent chunk identities and loose-object
   compatibility until those paths are implemented and measured.

Completion evidence must include the full workspace/platform gates required by
`AGENTS.md`, deterministic format and corruption coverage, a real end-to-end
CLI run through the proactor path (local ML-DSA send/fetch and representative full-history
import/resume regression tests pass; canonical 472-commit dev import, closure
verification, first-parent order, per-version tree/blob parity and runtime-API checkout
of every historical version pass; literal per-version CLI invocations and SSH transfer
CLI scenarios remain open), and target-hardware thermal measurements.
Compilation alone does not prove asynchronous behavior or thermal safety.
