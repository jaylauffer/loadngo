# `loadngo-persistence`

Date: 2026-08-29

## What this is

`loadngo/persistence` (package `loadngo-persistence`) provides two
functions — `write_atomic` and `read_checked` — for writing and reading a
single, repeatedly-overwritten save file safely: atomic (a crash mid-write
never corrupts the existing file) and corruption-checked (a bit flipped at
rest, or a write that partially landed, is detected on the next read rather
than silently accepted).

It pairs with `loadngo_host_desktop::app_data_dir(app_id)` (all six
platform backends), which already solves *where* to put a writable file.
This crate solves *how* to write one safely, independent of platform.

## Why a new crate instead of `loadngo/data`

`loadngo/data`'s own doc comment scopes it to "the Rust port of loadngo
Task," and it depends on `qcoin-crypto`. It also already has two
almost-this: `cas.rs`'s private `save_index`/`load_index` (write-to-temp
then rename, but no corruption or version check on read) and the crate's
`persistence` module (`write_task_file`/`read_task_file`, which writes a
`version` field that's never validated on read and isn't written
atomically). Neither is a generic, reusable primitive, and pulling in
`loadngo/data` for a game that just needs to save a profile would also
pull in the qcoin/Task dependency footprint for no reason.

## Why not `host-core`/`host-desktop`

Those crates own platform *integration* — input, windowing, rendering, and
now `app_data_dir` for path resolution. Atomic-write-then-rename and BLAKE3
hashing need zero OS-specific branching (`std::fs::rename` is atomic on the
same volume on every platform this crate targets, including Windows via
`MoveFileExW`/`MOVEFILE_REPLACE_EXISTING`). Putting this in host-desktop
would mean six copies of the same platform-independent logic.

## Prior art this borrows from

The hash-verify half of this isn't new — it's the third generation of the
same idea in this codebase family:

1. `loadngo-cpp/CAS/cas.cpp` — `GetHash` (MD5) + `VerifiedReadAll`,
   re-hashes on read and rejects a mismatch.
2. `loadngo/data/src/cas.rs` — the Rust rewrite, same idea with BLAKE3
   (`CasHash::digest`, `verify_and_add`, `verified_read_all`).
3. `entitlement-achievement-blockchain/eab-core`'s
   `FileOfflineAchievementStorage` — SHA-256 over each record with its own
   hash field cleared, checked on load.

All three verify *immutable* content after the fact — CAS content is
content-addressed and never overwritten in place, and EAB records are
appended, never rewritten. None of them needed to solve atomicity for a
single file that gets overwritten repeatedly, which is what a save/profile
file actually is. That's the piece this crate adds; the hash-verification
idea itself is inherited, not reinvented.

`sng-rusty/src/runtime/mod.rs`'s several save/load pairs (`save_story_slot`,
`save_jbones_wallet_to_path`, `load_chain_state`, ...) were checked too and
have the same gap: plain `std::fs::write`/`read`, no temp file, no
checksum, and a `version` field that's present but never validated.
Nothing there was reusable either.

## On-disk format

```text
offset 0..4    magic b"LGP1"
offset 4..36   BLAKE3 hash (32 bytes) of bytes[36..]
offset 36..40  caller-supplied schema_version: u32, little-endian
offset 40..    payload bytes (opaque to this crate)
```

The magic distinguishes "not one of our files at all" from "one of our
files, but corrupted" — the same idea as the CAS lineage's own `Marker`
constant, just at the file-envelope level instead of the content-block
level.

## Design choices

- **Byte-oriented, not generic over `serde::Serialize`.** Callers own their
  serialization format (RON, JSON, anything). Keeps this crate's only real
  dependency `blake3` — no format opinion, no `serde` dependency.
- **Schema-version policy is the caller's job.** This crate reports the
  stored `schema_version` cleanly; it does not decide what counts as too
  old, too new, or migratable. `sng-roguelite`'s `LocalProfile` already has
  its own `validate()` doing exactly that for its own schema — this crate
  is the missing storage layer underneath it, not a replacement for it.
- **No dependency on `anyhow`/`thiserror`.** Matches `ui-core`/`host-core`/
  `touch`'s existing convention for small library crates: a hand-rolled
  error enum with manual `Display`/`Error` impls.

## What's not done yet

Nothing in `loadngo`, `sng-roguelite`, or any other consumer calls this
crate yet. Wiring `sng-roguelite`'s `LocalProfile` to actually persist
through it is the natural next step, but a separate decision (save
path/filename, when to write, how `LocalProfile::validate()`'s existing
schema check composes with this crate's version field) left for its own
pass.
