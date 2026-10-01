# Loadngo Archive CAS v1

`data::cas::CasStorage` is the packet-oriented CAS used by the current
Loadngo network path. Its blob sizes use `u32`, so it remains intentionally
small-object compatible.

Archive CAS is a separate local-preservation layer for full disks, art source,
and other large material. It uses bounded-memory streaming, `u64` object
sizes, immutable objects, and a deterministic archive root. It does not change
the network packet format.

## Layout

An archive root has this layout:

```text
loadngo-archive-cas/
  objects/ab/<blake3-hex>.blob     stored as it is
  objects/ab/<blake3-hex>.zst      or compressed (see Compression)
  partials/<resume-key>.part
  ingest/<archive-id>.jsonl
  manifests/<archive-id>-<blake3-hex>.json
  compression.json                 present when new objects are compressed
  compression-kept-raw.txt         objects a compression pass left as they are
  compression-progress.json        how far the last compression pass got
```

Each object is addressed by its BLAKE3-256 digest. A manifest is both written
as a readable JSON receipt under `manifests/` and stored in `objects/` under
the same digest; that CAS object is the archive root. A manifest can also
record a source entry that was enumerated but could not be reopened.
Such an entry has no blob object and makes the capture explicitly incomplete.
An owner can later replace only an `unreadable` entry with a declared
`excluded` entry. That creates a new immutable manifest root made from the
unresolved root; it never edits or deletes the original receipt.

The manifest records only paths relative to the captured source root. It does
not record the source mount path or the staging mount path.

### How a version was made (manifest v3)

Since 2026-10-01 every new manifest is `loadngo-archive-manifest-v3`. Before its
entries it lists:

- `parents`: the versions it was made from. A fresh capture has none, an edit
  one, a merge one per source archive.
- `records`: the changes that made it from them, each with its kind, the paths
  it named, the actor, the time and the reason: `created`
  (`archive_cas_add`), `changed` (`archive_cas_exclude`), `moved` (a merge:
  each parent's tree under its folder), `deleted` (`archive_cas_remove`, the
  browser's Remove; a directory is one path) and `derived` (zips unpacked into
  folders at the same path).
- `unverified_history`: files kept as they were found. These are the sidecar
  logs written before v3, attached by `archive_cas_upgrade`.

All three are inside the root, so the hash and the signature cover why a
version exists as well as what it holds; listings read them from the header
without the entries. Before v3, a manifest named at most one
`supersedes_archive_root`, and the reason sat in a `.delete-log.json`,
`.add-log.json`, `.merge-log.json` or `.unpack-log.json` beside it, outside
the hash. Those manifests still read and verify byte for byte; nothing writes
the logs any more. The model this follows, shared with Task sync, is
[RECONCILIATION.md](RECONCILIATION.md).

## Ingestion guarantees

- Source files are streamed with an 8 MiB buffer; the archive does not hold a
  large input file in memory.
- An object is published by a filesystem hard link only after its temporary
  file has been synchronized. Existing content-addressed objects are never
  overwritten.
- A restart can reuse a partial when its relative source key, size, and
  modification time match. Before appending, Archive CAS byte-compares and
  re-hashes the whole existing prefix against the source.
- Each finished regular file is synchronously recorded in an append-only
  ingest journal. A later run with the same archive id and source label skips
  journaled files only when their source size and modification time still
  match and their published object is present at the expected size. The final
  verifier still re-hashes every object.
- The source file is statted before and after ingestion. A changed source
  fails instead of entering the manifest as though it were a stable capture.
- Equal files deduplicate to one object while retaining distinct paths in the
  manifest.
- Directories and symlinks are represented in the manifest. Unsupported
  special files cause ingestion to stop rather than silently omitting data.
- Since 2026-10-01 a directory is recorded only when something is stored
  below it: an empty directory leaves no entry. To keep one, put a `.keep`
  file in it. A directory holding a valid `CACHEDIR.TAG` is regenerable build
  output and is skipped whole: Cargo tags every target directory, whatever it
  is named (`target-android-build-std` too). Both are printed as they are
  skipped and counted in the summary. `archive_cas_add` follows the same rules
  inside a folder, but always records a folder named on its command line.
- A `NotFound` entry returned immediately after directory enumeration is
  recorded as `unreadable` with its failed operation and OS error. Ingestion
  continues so every accessible file is preserved, but the manifest is marked
  incomplete and the verifier exits non-zero after verifying the captured
  blobs. Recover that entry with a different filesystem reader before claiming
  a full preservation copy.
- An explicit owner-approved exclusion is a scoped, auditable decision rather
  than a recovery claim. It permits successful verification of every retained
  blob and reports `complete within declared scope`, but it does **not** mean
  every byte from the original source was captured.

- Zips (`.zip`, `.ipa`, `.jar`, `.apk`, and zips inside them up to three levels)
  are unpacked into folders of their members after the capture, so their
  contents are stored and deduplicated like any other file (since 2026-09-30;
  `--keep-zips` stores them whole). The unpacked version is made from the
  capture by a `derived` record naming each zip; a zip that cannot be
  unpacked stays whole and is listed in the summary. Nothing is deleted: the zips' own
  bytes stay until the capture version is purged. Office documents stay whole.
  This is the same unpack as `archive_cas_unpack`, which does it for archives
  captured before.

The source is never changed by ingestion. Archive CAS has no automatic partial
garbage collection; keep partials until a verified archive exists, then decide
on cleanup as a separate, explicitly authorized maintenance operation.

## Compression

Since 2026-09-30 an object may be stored compressed with zstd as
`objects/<xx>/<hash>.zst` instead of `<hash>.blob`. Compression changes only
how bytes sit on the disk: an object is still named by the BLAKE3 hash of its
**uncompressed** bytes, manifests still record its uncompressed size, and
roots, signatures and dedup are unchanged. Every reader (`verify_object`,
`read_range`, restore, the browser's preview, Kimi's `cas_*` tools, unpack,
purge and GC) takes either form and checks the uncompressed bytes against the
hash. Builds from before this change cannot read `.zst` objects: rebuild the
tools and the browser before using a compressed root.

- `archive_cas_compress --cas-root R --enable` writes `compression.json`
  (zstd level, default 9) so every new object is stored compressed, then
  compresses the objects already stored. `--settings-only` only changes the
  setting; `--disable` turns it off (stored objects keep their form).
- `--dry-run` compresses into scratch files and reports the saving without
  changing anything; `--max-gib N` bounds it to a sample.
- Each `.blob` is hashed while it is compressed, so a damaged one is reported
  and left alone. The `.zst` is published only after it decompresses back to
  the same hash, and the `.blob` is removed after that; a stop in between
  leaves both, and the next run finishes the job.
- Objects under 8 KiB stay as they are (block rounding eats the saving), and so
  do objects that compress by less than a sixteenth: a 1 MiB sample of large
  objects is tried first so media and archives are skipped quickly. Those
  are listed in `compression-kept-raw.txt` and not retried.
- A pass (not a dry run) records how far it has got in
  `compression-progress.json` once it has listed the root's objects, about every
  10 s after that, and when it ends: objects at the start, objects done, compressed
  and kept, bytes saved, its pid, and whether it finished. Its log line reads
  `N of M objects (P%)`. Kimi's `cas_archives` shows the record, and calls a pass
  with no update for two minutes stopped. Passes started before 2026-10-01 write
  no record.
- Reading from an offset in a compressed object decompresses from its start;
  reading a compressed zip for unpacking decompresses it to a temporary file.

Measured 2026-09-30 with `--dry-run --max-gib 8` on `loadngo-archive-cas`
(Loadngo Archive Staging, a USB "My Passport" drive; 12,413 objects in hash
order, i.e. a random sample, 8.03 GiB): 8,507 objects were under 8 KiB, about
840 did not compress, and about 3,060 did. Level 3 would save 3.88 GiB (48.2%
of the bytes examined) at 8.4 MiB/s; level 9 would save 4.05 GiB (50.4%). The
level 3 run read the drive cold, so its rate is the drive's: random reads
(seek-bound), CPU about 12% busy. At that rate a full pass over this 733 GiB
root takes about a day; the level 9 rate (13.7 MiB/s) was measured on a warm
page cache and is not comparable.

First real root, 2026-09-30, at Jay's request: `pudding-cas` on Zhoenus II
(archive `pudding-20260917`, a USB SSD), `--enable --level 3`: 83,731 objects
(47.69 GiB) examined in 637 s (76.6 MiB/s); 31,369 compressed, 19.91 GiB to
7.97 GiB; 49,093 under 8 KiB and 3,269 incompressible kept as they were; 0
failures. The volume went from 49 to 37 GiB used. Afterwards
`archive_cas_verify` re-hashed all 83,730 unique objects (51.1 GB
uncompressed, 60 s; capture complete) and `archive_cas_sign verify` accepted
the signature against the trusted key.

Build output removed from `pudding-20260917`, 2026-10-01, at Jay's request:
`archive_cas_remove` took out `build_tmp` (March tarball exports),
`.loadngo-cas` (the March v0 pudding CAS), every `target-android-build-std`,
the app bundles, harnesses and packages under `*/build/`, and
`sng-rusty/.venv` (146,000 files, 38.7 GiB). Small hand-kept parts of `build/`
stayed (`sng-rusty/build/loadngo-cas`, `playlists`, `voice-audit`,
`qcoin/build/qcoin-quorum`). The new root `4d8babf2...` (11,901 files, 24.2 GiB)
verified complete, was signed by `jay-macmini`, and replaced `d8ec110f...`
(its delete log was kept by hand); `archive_cas_gc --execute` removed 76,332
objects, 14.16 GiB, and the volume went from 37 to 22 GiB used. A second
`archive_cas_verify` afterwards: 7,399 objects, complete. Pruning deleted the
`d8ec110f` manifest, signature and stored manifest object, so that version
can no longer be checked; that is why `archive_cas_prune_manifests` was
removed the same day (see "Changing an archive" below, and the lessons in
[PUDDING_CAS_PQ_MODEL.md](PUDDING_CAS_PQ_MODEL.md)).

## Commands

Every command below, and `archive_cas_sign` and `archive_cas_browser`, prints
its full flag reference -- with a description and required/optional marker
for every argument, plus at least one worked example -- when run with
`--help`/`-h`, or with no arguments at all if it has any required argument.
See [`CLI_CONVENTIONS.md`](CLI_CONVENTIONS.md) for the convention every
loadngo tool follows; the examples here are the short version.

Create an archive from a mounted read-only directory:

```sh
cargo run -p data --bin archive_cas_ingest -- \
  --source /path/to/read-only-source \
  --cas-root /path/to/writable/loadngo-archive-cas \
  --archive-id source-archive-yyyymmdd \
  --source-label "Human-readable source label"
```

The command prints the manifest file and its archive-root hash. If it is
interrupted, rerun the identical command; matching partial files resume after
prefix verification.
When the source holds zips it also prints the unpacked version, which is the
archive's current manifest.

Try compression on a sample, then turn it on and compress what is stored:

```sh
cargo run --release -p data --bin archive_cas_compress -- \
  --cas-root /path/to/writable/loadngo-archive-cas --dry-run --max-gib 20
cargo run --release -p data --bin archive_cas_compress -- \
  --cas-root /path/to/writable/loadngo-archive-cas --enable
```

Create a successor manifest that excludes one already-recorded unreadable
entry without rereading the source or modifying the original manifest:

```sh
cargo run -p data --bin archive_cas_exclude -- \
  --cas-root /path/to/writable/loadngo-archive-cas \
  --manifest /path/to/writable/loadngo-archive-cas/manifests/<unresolved>.json \
  --only-unreadable \
  --reason "owner-approved source exclusion" \
  --actor jay
```

`--only-unreadable` succeeds only when the manifest has exactly one unresolved
entry. Use `--path` when there are multiple entries.

Verify the root receipt and every referenced content object:

```sh
cargo run -p data --bin archive_cas_verify -- \
  --cas-root /path/to/writable/loadngo-archive-cas \
  --manifest /path/to/writable/loadngo-archive-cas/manifests/<archive>.json
```

The verifier re-hashes each distinct object once, in the first manifest order
that referenced it. That preserves the archive's original ingestion order on
magnetic media while avoiding repeat reads for deduplicated paths. It reports
both logical source bytes and unique stored object bytes; the latter is the
data read from the archive volume.

Restore a deliberately selected set of regular files into a **new** directory:

```sh
cargo run -p data --bin archive_cas_restore -- \
  --cas-root /path/to/writable/loadngo-archive-cas \
  --manifest /path/to/writable/loadngo-archive-cas/manifests/<archive>.json \
  --destination /path/to/empty-new-restore-directory \
  --path relative/path/to/one-file \
  --path relative/path/to/another-file
```

Restore refuses an existing destination root and any existing file. Each output
is copied through a temporary sibling, then BLAKE3-checked against its archive
object before it is published. This is intentionally a narrow recovery drill:
it restores selected regular-file paths only, does not recreate symlinks, and
does not yet reapply timestamps or ownership. `--path` also takes the name of
an attachment, to get an old sidecar log back from a version that keeps it.

Move an archive's old sidecar logs into the store (each archive once):

```sh
cargo run -p data --bin archive_cas_upgrade -- \
  --cas-root /path/to/loadngo-archive-cas --archive <archive-id> \
  --attach /path/to/loadngo-archive-cas/manifests/<log>.delete-log.json --dry-run
```

It writes a v3 version with the same entries, made from the current one, that
carries the logs unchanged; once their stored copies verify, the log files in
`manifests/` are deleted. The new version is unsigned.

## Changing an archive

A change never edits a version in place: `archive_cas_remove`,
`archive_cas_add`, `archive_cas_exclude`, `archive_cas_unpack`,
`archive_cas_merge` and the browser write a new version that names the
version or versions it was made from, with a record of the change. Freeing the space a removal leaves is a
separate, deliberate step:

1. Sign the new version (`archive_cas_sign sign`, or the browser's Sign).
2. `archive_cas_purge --cas-root R` prints the plan;
   `--execute <plan-id>` carries it out. A superseded version is **retired**
   once a later version of its archive is signed by the trusted key
   (`--trusted-public-key`, default: the one `*.dilithium2.pub` in
   `~/.loadngo/keys`). A merge's sources are retired the same way under the
   signed merged archive. A retired version keeps its manifest, its stored
   manifest object, its signature, its attachments and any old logs; only the
   objects no live
   version lists are deleted. A superseded version without a signed later
   version is listed as not retired, and nothing of it is deleted.
3. `archive_cas_verify` on a retired version verifies every object it still
   has and counts the rest as dropped on retirement, naming the signed later
   version.

So the history of an archive stays: every version it had can be listed,
checked against its signature and verified, even after its unique bytes are
gone. Only `archive_cas_purge --delete-archive` deletes manifests, every
version of that one archive. `archive_cas_gc` counts every manifest on disk,
retired ones included, so it frees only strays an interrupted run left.

## Scope and future promotion

This is an integrity and recoverability layer, not an authenticity claim.
BLAKE3 provides content identity and corruption detection; a later promotion
step can bind an archive root to Loadngo/QCoin policy and a post-quantum
signature without placing private source paths or file contents on the task
plane.

The current physical staging volume is intentionally unencrypted at the
owner's direction. Treat physical custody as part of the archive's privacy
model, and do not expose its manifests or source contents through shared task
traffic.
