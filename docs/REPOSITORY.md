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
| Version | `loadngo-repository-version-v1` | repository id, parents, root tree, author, author time, message, the changes from the first parent (`created`, `changed`, `deleted`, `moved` paths; a folder created or deleted whole is one path) and, for an imported commit, its origin (git commit id, committer, commit time) |
| File | — | the file's bytes, as Archive CAS stores them (zstd when the root compresses) |

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
(NTFS's short name for `.git`), so a checkout can never plant either.

## What a signature proves

A **head** is `repositories/<id>/heads/<branch>.json`: a claim that branch `<branch>` of
repository `<id>` is at version `<hash>`, with the signer's identity and time, signed with
Dilithium2 (`loadngo-pq-crypto`) inside a `loadngo-anchor` frame of domain
`loadngo.repository.head`, so it cannot be replayed as any other kind of signature. Every
claim ever made is also kept in `repositories/<id>/signatures/`.

A version is **trusted** when it is, or is an ancestor of, the version of a head whose
signature verifies against a trusted key (`--trusted-public-key`, or the one
`*.dilithium2.pub` in `~/.loadngo/keys`). Every object is hash-checked as it is read, so
a trusted head vouches for every byte below it. `checkout`, `log`, `commit`, `send` and
`receive` refuse anything not trusted; `verify` re-hashes a branch's whole history.
`init`, `import` and `commit` also refuse to sign with a key the trusted keys do not
hold, which would leave the branch where no later command could move it.

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
| `verify --repository R --branch B` | the head's signature, then every version, tree (with its totals) and file hash |
| `send --repository R --branch B (--to <store> \| --to-ssh <host> --remote-store <folder> [--remote-program <path>])` | sends what the other store lacks; it checks everything and moves its branch forward |

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
signed head. The receiver hashes every object as it stores it, verifies the head's
signature against its own trusted key, checks every object below it, and moves its
branch only forward. Over ssh the key exchange is pinned to the post-quantum hybrids
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

- **Merging and reconciling.** Versions may have several parents, but nothing makes a
  merge; `commit` refuses when the branch moved. This is
  [RECONCILIATION.md](RECONCILIATION.md) step 2 onward.
- **Fetching.** `send` pushes; nothing pulls yet, and nothing moves an existing checkout
  to a newer version (check out again).
- **Deleting a branch**, partial checkouts, and a content `diff` command.
- **Symlinks on Windows** (checkout refuses them); Windows executable bits come from the
  version.
- **Keys and trust:** author and approver signatures, a trust-root store, rotation,
  rollback protection (above).
- **Fewer syncs:** one sync per batch of objects instead of per object.
- **Large files:** import holds one git blob in memory at a time; checkout, commit and
  transfer stream.
- **Archive manifests** are still one flat list per version; only repositories have
  directory nodes.
