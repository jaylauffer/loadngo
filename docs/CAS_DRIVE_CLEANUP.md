# Cleaning the Mac mini's drive with Kimi and the Archive CAS: plan

Status: plan, 2026-09-26, at Jay's request ("I will need Kimi to clean the Mac drive
utilizing the CAS archives"). Nothing here is built yet. Kimi's file tools are read-only
today (`LOCAL_MODEL_CAS_TOOLS.md`).

## The drive today

The internal drive (926 GB APFS container) had 20 GB free on 2026-09-26: 98% full.
Measured with `du`/`df` the same day:

| What | Size | Kind |
|---|---|---|
| Rust `target/` directories under `~/pudding` | 98 GB | regenerable build output |
| `~/Library/Developer/Xcode/iOS DeviceSupport` | 68 GB | regenerable (Xcode re-fetches for a connected device) |
| `~/Library/Developer/Xcode/DerivedData` | 41 GB | regenerable build output |
| `~/Library/Caches`, `~/.cache`, `~/.gradle`, `~/Library/Android` | 53 GB | caches and SDKs, mostly regenerable |
| `~/dolores-card-20260916.img` | 63 GB | data: the verified image of dolores's old card (the old card is itself the rollback) |
| `~/20260913-pudding-backup.tar.gz` | 6.4 GB | data: a pudding backup that predates the signed pudding CAS |
| `~/Music` | 118 GB | Logic sound library 73 GB (Apple content, re-downloadable); music 26 GB and Logic projects 19 GB (Jay's own work) |
| `~/Downloads`, `~/Documents`, `~/Movies`, `~/Pictures` | 44 GB | data |
| `~/pudding` sources, excluding `target/` | about 70 GB | data: repositories (on GitHub) and the rest of the workspace |

Archive space: Zhoenus II (holds the signed pudding CAS) has 883 GB free; Loadngo
Archive Staging has 1.1 TB free.

## The rule

**Nothing is deleted on the model's judgement alone.** A file may be removed only when
a deterministic check proves one of:

1. **Archived.** Its exact bytes are in an Archive CAS root whose manifest is signed by
   the trusted key and verifies. The check is BLAKE3 of the local file against the
   manifest entry and a present, verified object. This is the same trust path as
   `archive_cas_verify` and `cas_read`.
2. **Regenerable.** It sits inside a directory of a known regenerable kind: a Cargo
   `target/` next to a `Cargo.toml`, Xcode `DerivedData`, `iOS DeviceSupport` for an
   iOS version other than the attached iPhone's, or a named cache directory. This needs
   no archive, and it is Jay's call whether Kimi handles it (see Decisions).

Anything else stays where it is.

## Who does what

- **Kimi** investigates and proposes. It uses the read-only tools, plus two new
  read-only ones: `fs_usage`, which gives directory sizes, and `cas_check`, which
  answers "is this file, or this directory, archived, where, and under which signed
  root?". It explains each proposal in plain words: what the files are, which archive
  holds them, and how much space they free.
- **A deterministic tool** does the verifying, archiving and deleting. The model never
  decides that bytes match.
- **Jay** approves each batch in the chat. Kimi presents the batch, and the tool
  deletes only when Jay answers yes.

## New tools

| Tool | Kind | What it does |
|---|---|---|
| `fs_usage` | read-only | Sizes of a directory's children, bounded (depth 1, at most 200 entries). |
| `cas_check` | read-only | For a local path, hashes each file (BLAKE3) and looks it up across the trusted CAS roots. Reports archived / not archived / changed since archived, per file and in total, with the archive id and signed root. Bounded per call; a large directory reports a summary and continues on the next call. |
| `cas_archive` | writes to an archive drive | Ingests a local directory into a CAS root with the existing `archive_cas_ingest`, then runs `archive_cas_verify`. Signing stays with Jay's key and Jay's command (`archive_cas_sign`) unless he decides otherwise. |
| `fs_remove` | destructive | Removes only paths that `cas_check` (or the regenerable rule) cleared in this session and that have not changed since (size and modification time rechecked, and re-hashed if either differs). Needs Jay's yes in the chat for that exact batch. Logs every removal (path, size, hash, archive root) to a removal log on the archive drive. |

The destructive tool would be the first write capability Kimi has. It would be on only
when the launcher asks for it (for example `--cleanup`), never by default.

## Order of work

1. `fs_usage` and `cas_check` (read-only). Kimi can then report what is and is not
   archived, with nothing at risk.
2. `cas_archive`: archive what is not yet archived onto Zhoenus II (music and Logic
   projects, Documents, Pictures, Movies, Downloads, the dolores image), then sign
   with Jay's key.
3. `fs_remove` with approval in the chat, and the removal log.
4. A first supervised session. Start with the largest verified items: the dolores
   image once it is archived, then the pudding backup tarball, and the regenerable
   build output if Jay includes it.

Kimi's speed matters here. Reading tool results is prompt processing, which is the part
the GPU work has not yet made fast (`METAL_COMPUTE_PLAN.md`). Hashing is done by the
tool, not the model, so large directories cost tool time, not model time.

## Decisions for Jay

- **Regenerable build output and caches** (about 220 GB). Should Kimi propose these
  too, or only archived data? Clearing them is the quickest way to space and needs no
  archive. The cost is rebuild time: a full Rust rebuild after `target/` is removed,
  and Xcode re-fetches device support.
- **The destination archive.** Zhoenus II, next to the pudding CAS; or Loadngo Archive
  Staging.
- **Music.** Is the 73 GB Logic sound library in scope? It is re-downloadable from
  Apple, so it needs no archive.
- **Signing.** Does it stay manual (Jay runs `archive_cas_sign`), or may the tool sign
  with the local key?
