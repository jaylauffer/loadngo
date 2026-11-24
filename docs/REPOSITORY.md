# Repositories: post-quantum version control on the Archive CAS

Status, 2026-11-04 (Claudette, Task `task-records/2026-11-04-pq-repository/`):
`loadngo-repo` (`data/src/bin/loadngo-repo.rs`, library `data::repository`) holds
loadngo's whole `dev` history as signed versions. Measured on the Mac mini:
465 commits imported in 79 s into a 93 MB store; `verify` re-hashes all 465
versions, 2,058 trees and 2,240 files (79.6 MB) in 0.3 s; a checkout of the head
(779 files, 29.5 MB) takes 0.1 s and is byte-for-byte `git archive` of the same
commit. The Task's check (`verify.sh`, pushed before the work) runs these claims
against every commit, and on dolores.

This is the "git replacement" step of [PUDDING_CAS_PQ_MODEL.md](PUDDING_CAS_PQ_MODEL.md):
content-addressed storage, explicit ancestry, post-quantum signatures, directory nodes
(lesson 3 there; step 3 of [RECONCILIATION.md](RECONCILIATION.md), for repositories).

## What a version is

Three kinds of object, each the canonical JSON of its fields, stored in an Archive CAS
root's `objects/` and named by the BLAKE3-256 hash of its bytes:

| Object | Format | Holds |
|---|---|---|
| Tree | `loadngo-repository-tree-v1` | one folder: entries sorted by name, each `file` or `executable` (object hash and size), `symlink` (target) or `directory` (tree hash, with the files and bytes below it) |
| Tree | `loadngo-repository-tree-v2` | the same directory entries plus `chunked_file` (manifest object, logical whole-file hash and size, executable bit) |
| Tree | `loadngo-repository-tree-v3` | v2 entries, written by runtime-aware commands using whole-file CAS objects for newly ingested regular files below the CDC minimum (8 KiB) and chunk manifests for larger files; unchanged entries from earlier trees may retain their existing representation |
| Version | `loadngo-repository-version-v1` | repository id, parents, root tree, author, author time, message, the changes from the first parent (`created`, `changed`, `deleted`, `moved` paths; a folder created or deleted whole is one path) and, for an imported commit, its origin (git commit id, committer, commit time) |
| File | — | v1/v3 raw entries: the file's bytes as one Archive CAS object; v2/v3 chunked entries: a canonical CDC manifest whose ordered chunk references name immutable Archive CAS objects and whose whole-file BLAKE3 hash and size identify the reconstructed bytes |

New chunked files use the versioned `loadngo-buzhash64-cdc-v2` profile (64-byte
rolling Buzhash with a correctly rotated outgoing byte; 8/64/128 KiB
minimum/target/maximum). Canonical manifests still accept the earlier v1 profile.
Their boundaries remain readable because manifests reference explicit chunk identities.
Existing v1 and v2 trees remain valid. V2 and v3 trees may hold both raw and
chunked files, which lets later commits retain unchanged chunk manifests while
replacing changed files. Runtime-aware import/init/commit use v3 so the raw-file
cutoff is explicit in the persisted tree format.
Verification, send planning, receive closure checks, checkout, and repository GC follow
the manifest and all referenced chunks. Import spools each Git blob through a fixed
1 MiB buffer, then chunks and stores it through proactor-backed CAS I/O. Import storage
and `verify` use the command-owned proactor and thermal admission. Checkout tree/manifest
reads and CAS object restores use the proactor; send payload reads, receive's
temporary-file-to-CAS ingestion, and `init`/`commit` changed-file ingestion use admitted
CAS paths. Status/init/commit hash working files through thermally admitted proactor reads,
processing each bounded buffer on the repository's bounded CPU worker lane.
Regular files below 8 KiB, including empty files, are stored whole; larger newly
ingested files use chunk manifests. Runtime-aware writers persist this selection
under tree v3. CDC boundary detection and hashing also yield to thermal admission
every 64 KiB.
New head signing and trusted-head checks on runtime-aware CLI paths use the thermally
admitted bounded CPU worker. Runtime status traversal and ignore-file reads, checkout
directory/symlink/mode changes and publication, branch-head publication, and local
`receive` framed stdin reads use bounded blocking-I/O or worker lanes. Send pipe writes
and fetch reads also use the bounded I/O lane. Synchronous API fallbacks, CAS range
metadata checks outside runtime checkout, filesystem preflight checks outside restore,
non-runtime tree diff, and some version validation remain synchronous. Checkout CAS
source/output open/close, source metadata checks, destination preflight/sync, and
publication use the bounded blocking-I/O lane. Incoming payload hashes use the CPU worker, and partial-file writes/sync
apply thermal admission at bounded steps. See
[REPOSITORY_RUNTIME.md](REPOSITORY_RUNTIME.md) for the current integration boundary.

A version names its parents and tree by hash, and a tree names what it holds by hash,
so one hash pins a whole history. A version holds no signature, store path or import
time, so the same history gives the same hashes on every machine: importing `dev~7`
and then `dev` gives exactly the versions of importing `dev` at once.

A change to one file writes new trees only along that file's path. A commit to
`docs/REPOSITORY.md` writes the file, two trees (`docs/` and the root) and a version,
a few KB, where an archive manifest lists every entry again.

Names a tree may not hold: empty, `.`, `..`; anything with `/`, `\`, `:` or NUL, or
ending in a dot or space (path syntax or aliases on Windows); `.git` and
`.loadngo-repo` in any case, with HFS+'s ignored code points removed, and `git~1`
(NTFS's short name for `.git`), so a checkout can never plant either. Windows device
names (`CON`, `NUL`, `COM1`... with or without an extension) and `<>"|?*` may be held
(git on Linux has them, e.g. `aux.c`); a checkout on Windows refuses them.

## What a signature proves

A **head** is `repositories/<id>/heads/<branch>.json`: a claim that branch `<branch>` of
repository `<id>` is at version `<hash>`, with the signer's identity and time, signed with
ML-DSA-65 (`loadngo-pq-crypto`) inside a `loadngo-anchor` frame of domain
`loadngo.repository.head`, so it cannot be replayed as any other kind of signature. Every
claim ever made is also kept in `repositories/<id>/signatures/`.

**Algorithm compatibility:** new keys default to standardized FIPS 204 ML-DSA-65, from the
pure-Rust `fips204` crate (0.5.0, pinned; until 2026-11-24 from AWS-LC, whose keys and
signatures it matches: see REPOSITORY_RUNTIME.md). The repository scheme ID is distinct (`0x03`), and the
existing length-prefixed key/signature envelope remains unchanged. Signing uses pure
ML-DSA with an empty context. Legacy `Dilithium2` (`pqcrypto-dilithium` 0.5.0, pre-final
round-3 parameters) and `Falcon512` remain available for explicit verification; neither
is relabeled as ML-DSA.

A version is **trusted** when it is, or is an ancestor of, the version of a head whose
signature verifies against a trusted key (`--trusted-public-key`, or the one `*.pub`
file in `~/.loadngo/keys`). Every object is hash-checked as it is read, so
a trusted head vouches for every byte below it. `checkout`, `log`, `commit`, `merge`, `send` and
`receive` refuse anything not trusted; `verify` re-hashes a branch's whole history.
`init`, `import`, `commit` and `merge` also refuse to sign with a key the trusted keys do
not hold, which would leave the branch where no later command could move it.

What it does not prove yet:

- **Rollback.** An old head file, signed in its day, still verifies. Each store moves a
  branch only forward on its own (`receive` and `commit` refuse anything else), but a
  person with write access to the store folder can put an older signed head back.
- **Who did the work.** Every head so far is signed by this Mac's key (`jay-macmini`);
  the version's `author` field names who made it. Author and approver keys are step 5
  of [RECONCILIATION.md](RECONCILIATION.md).
- **Trust policy.** One trusted key, named per command. No rotation, revocation or
  per-repository signer list yet.

Hashes are BLAKE3-256 (128-bit security against a quantum search). The formats name
the algorithm by their version string; a later format can change it without touching
old objects.

## Commands

`loadngo-repo --help` documents every flag. The store defaults to
`$LOADNGO_REPO_STORE`, else `~/.loadngo/store`.

| Command | Does |
|---|---|
| `import --repository R --branch B --from-git <folder> [--git-ref REF]` | every commit reachable from REF (default B) becomes a version, oldest first; resumes from the commit the branch was last imported at; signs the new head |
| `init --repository R --branch B --tree <folder> --message M --author A` | records a folder (not ignored files) as the first version of a new branch; the folder becomes its checkout |
| `log --repository R (--branch B \| --version V)` | first-parent history, newest first: version, git commit or `-`, parents or `-`, time, author, subject (tab-separated) |
| `checkout --repository R (--branch B \| --version V) --destination <new-folder>` | writes the version, checking every file against its hash, into a temporary sibling renamed into place at the end: a refused checkout leaves nothing |
| `status [--tree <folder>]` | `created`, `changed` or `deleted` and a path for each difference from the tree's version |
| `commit [--tree <folder>] --path P... --message M --author A` | records the changes under the named paths as a new signed version; refuses if the branch moved since the tree was based on it |
| `merge --repository R --branch B --other-branch O --message M --author A [--ours P...] [--theirs P...]` | reports when the source is already contained, otherwise fast-forwards or performs a three-way merge; small UTF-8 files combine non-overlapping line edits automatically, while unresolved conflicts use whole-file or subtree choices; verifies complete closure before signed head publication; existing checkouts remain unchanged |
| `diff --repository R --from-branch A --to-branch B` | path-level created, changed, deleted, moved, or derived entries between two trusted branch heads; does not print file contents |
| `verify --repository R --branch B` | the head's signature, then every version, tree (with its totals) and file hash |
| `send --repository R --branch B (--to <store> \| --to-ssh <host> --remote-store <folder> [--remote-program <path>]) [--all]` | sends what the other store lacks (with `--all`, everything, which repairs its damaged copies); it checks everything and moves its branch forward |
| `fetch --repository R --branch B (--from <store> \| --from-ssh <host> --remote-store <folder>)` | asks the source to stream the branch update locally or through PQ-constrained SSH; reuses the local branch's verified history when available, and verifies source exit, signature and full object closure before advancing the local branch |

A checkout holds `.loadngo-repo` at its top: the store (absolute path), repository,
branch and the version the tree is based on.

### Working trees

`status` and `commit` look at tracked paths always. Untracked paths are skipped when
the tree's `.gitignore` files ignore them (git's rules: `!`, `/` anchoring, folder-only
rules, `*`, `?`, `[...]`, `**`; not `.git/info/exclude` or global excludes), inside a
build cache (`CACHEDIR.TAG`) or another checkout, or named `.git`. Executable bits are
read where the platform has them. On a volume without them (exFAT or NTFS mounted on
macOS, where every file reads as executable) `status` reports every plain file as
changed; there is no setting like git's `core.fileMode` yet.

A checkout's own files (`.loadngo-repo` and its temporary) are created exclusively, so
a symlink in the tree cannot have them written outside the folder.

`commit` records only the paths named with `--path`, never the whole tree unasked:
several agents share checkouts in this workspace (`COLLABORATION.md`, rule 3).

### Import from git

`import` reads git through one `git cat-file --batch` process and converts each git
tree and blob once. Kept: files, executable bits, symlinks, parents (merges too), author
and message; the git commit id, committer and commit time as the origin. Not kept:
timezone offsets, gpg signatures and other extra commit headers. Refused: submodules,
names that are not UTF-8 or a tree may not hold.

### Between machines

`send` asks the receiver (`loadngo-repo heads`) what it has, then writes one stream to
`loadngo-repo receive` there: the missing file objects, trees and versions, then the
signed head. The receiver takes each object into a partial file and stores it only if
its bytes hash to the name it arrived under; a copy it already holds is replaced only
when that copy does not read back intact, so a stream can never remove a good object.
It then verifies the head's signature against its own trusted key, checks every object
below it (also when the branch is already there, so a damaged replica is reported, not
called up to date), and moves its branch only forward. Over ssh the key exchange is pinned to the post-quantum hybrids
`mlkem768x25519-sha256` and `sntrup761x25519-sha512@openssh.com`: macOS's ssh otherwise
picks `ecdh-sha2-nistp256` first for dolores (checked 2026-11-04), although both ends
support ML-KEM. Remote paths are absolute or relative to the remote home.

## Store

A store is an Archive CAS root: `objects/`, `partials/`, `manifests/` (unused by
repositories), `compression.json` when it compresses, and `repositories/<id>/`.
Repositories and archives can share a root: `archive_cas_gc` and `archive_cas_purge` keep
every object a repository needs ([`data::repository::live_objects`]: every version
reachable from a head or a kept claim, every tree and file below them), and refuse to
run if a repository cannot be read. Do not run either while a commit, import or receive
is writing to the same root.

**Builds of `archive_cas_gc`, `archive_cas_purge` or `archive_cas_browser` from before
`d9d3586c` do not know about repositories and would delete every repository object as
unreferenced.** Until every machine's copies are rebuilt, keep repositories in stores of
their own (`~/.loadngo/store`), not in shared archive roots such as `pudding-cas`.

Every object is written durably (synced) before a head names it. That costs: about 3
syncs per object, so the first import of loadngo spends most of its 79 s waiting on the
disk. Batching the syncs is open below.

## Not done yet

- **Textual merging and reconciliation.** Automatic line merging is limited to UTF-8
  files up to 512 KiB and a bounded diff table; binary, larger, overlapping, and
  file/directory conflicts require explicit whole-file or subtree choices. It does not
  emit conflict markers or reconcile content proposals. Histories with multiple best
  common ancestors still require recursive combination. See [RECONCILIATION.md](RECONCILIATION.md)
  step 2 onward.
- **Fetch efficiency and working-copy update.** SSH fetch uses the local branch tip as
  a hint only when its complete closure is intact; otherwise it requests the full history
  to repair missing or damaged copies. It advances the stored branch, but an existing
  checkout remains at its recorded version and must be checked out again.
- **Deleting a branch**, partial checkouts, and a file-content `diff` command.
- **Symlinks on Windows** (checkout refuses them); Windows executable bits come from the
  version.
- **Keys and trust:** author and approver signatures, a trust-root store, rotation,
  rollback protection (above).
- **Fewer syncs:** one sync per batch of objects instead of per object.
- **Large files:** import spools each Git blob through a fixed 1 MiB buffer. Checkout,
  commit and transfer use bounded CAS I/O. Runtime import/init/commit send CDC boundary
  and whole-file hashing to the bounded CPU lane. Git import traversal scheduling and
  zstd decoding remain synchronous on the command thread; the runtime checkout routes
  CAS source/output file lifecycle, destination preflight, durability, and publication
  through the bounded thermally admitted lane. Direct synchronous API fallbacks remain
  synchronous.
- **Archive manifests** are still one flat list per version; only repositories have
  directory nodes.
