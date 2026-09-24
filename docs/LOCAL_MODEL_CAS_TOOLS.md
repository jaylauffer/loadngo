# File access for local models: the local drive and the loadngo CAS

Status, 2026-09-24 (Claude Code): **built and used live** by Kimi Linear 48B-A3B. Jay:
"Kimi needs both basic file system access so she can read files on the local drive and
CAS capabilities"; the CAS is her default capability alongside ordinary read access.

## Two tool families, both read-only

- **Local drive (`fs_*`, `loadngo-inference::tools::FsTools`)**: ordinary read access,
  the established norm for coding agents. Relative paths start at `--fs-base` (the pudding
  folder from the launcher); absolute paths anywhere the user can read. Refused: key and
  credential stores (`~/.ssh`, `~/.gnupg`, `~/.loadngo/keys`, `~/.aws`, `~/.config/gh`,
  `~/Library/Keychains`) and files that look like private keys (`*.key`, `*.pem`, `id_*`,
  `.env`), checked after resolving symlinks. Walks skip `.git`, `target`, `node_modules`.
- **Signed snapshot (`cas_*`, `loadngo-inference::cas_tools` over
  `data::archive_view::ArchiveView`)**: the same kinds of reads against a signed Archive
  CAS snapshot, with provenance the live drive cannot give:
  - every file is BLAKE3-verified against the manifest before a byte reaches the model;
  - every result names the snapshot's signed root and signer, and `cas_read`/`cas_find`
    name each file's object hash, so any claim about a file can be checked by anyone
    holding the public key (`COLLABORATION.md` rule 5 applied to a model);
  - it answers about one identified state, not a checkout mid-edit.

Nothing in either family writes, deletes, executes or uses the network.

## What already exists (verified in `data/`)

- `archive_cas::ArchiveCasStorage`: `read_manifest` (rejects non-canonical JSON),
  `read_range(hash, offset, len)`, `verify_object` (re-reads and BLAKE3-checks a blob).
- `archive_cas_sign::verify_signature(signed, trusted_key) -> root` and
  `present_root(store, manifest)`: a snapshot is trusted only when the manifest's
  recomputed root equals a root signed by a trusted key.
- `cli::discover`: finds Archive CAS roots on attached storage.
- A signed workspace snapshot: `/Volumes/Zhoenus II/pudding-cas`, archive
  `pudding-20260917`, current root `d8ec110f...` (re-signed 2026-09-18 after a removal),
  157,876 files, 59 MB manifest; signing key `jay-macmini` (public key beside it).

That snapshot predates this week's work, so refreshing it (below) is part of the plan.

## CAS tools

Declared through the model's own tool format: Kimi Linear's `chat_template.jinja`
has a `tool_declare` system message, `<|tool_calls_section_begin|>` / `<|tool_call_begin|>`
calls and `## Return of <id>` results; its tokenizer has the five `<|tool_*|>` control
tokens as single ids (163595-163599). All read-only, all bounded:

| Tool | Arguments | Returns |
|---|---|---|
| `cas_list` | `path` (directory) | children with kind and size (hashes via `cas_read`/`cas_find`, to keep listings short); at most 200 entries |
| `cas_find` | `pattern` (path glob) | matching paths from the manifest; at most 100 |
| `cas_read` | `path`, optional `line_start`, `line_count` | UTF-8 text, at most 16 KiB per call; binaries report size and hash only |
| `cas_grep` | `pattern` (literal), optional `path_prefix` | matching lines with path:line, scanning at most 32 MiB of text per call |

Every result starts with `snapshot <id> root <hex> signed by <signer>`. The local tools
mirror them: `fs_list`, `fs_read` (16 KiB, line windows), `fs_find` (glob), `fs_grep`
(literal, 32 MiB scanned, 100 matches).

## Where the code is

- loadngo `inference/src/tools.rs`: `Tool`, `Toolbox` (JSON function declarations, calls
  by name), `FsTools`, glob and text helpers. No new dependencies beyond `serde_json`.
- loadngo `inference/src/cas_tools.rs` behind the `cas` feature, and
  `data/src/archive_view.rs`: a snapshot is trusted only when its signature verifies
  against the trusted key, the manifest it names is an intact CAS object, and that
  object's digest is the signed root. Unsigned manifests are never opened.
- kimi-k3-in-rust `crates/kimi-k3-cli/src/chat.rs`: Kimi Linear's `tool_declare` message,
  `<|tool_call_begin|>id<|tool_call_argument_begin|>args<|tool_call_end|>` parsing, results
  as `## Return of <id>` tool messages, at most 8 tool rounds per question, results
  shortened to fit the context. `k3` flags `--fs-base`, `--cas-root`, `--cas-key`,
  `--no-tools`; the launcher passes the pudding folder, the Zhoenus II snapshot when
  mounted, and the public key from `~/.loadngo/keys` (never the copy on the archive drive).

## Evidence, 2026-09-24

- Unit tests: filesystem read/find/grep with bounds, secret refusal, malformed calls;
  archive view refuses a tampered blob, an untrusted key and an unsigned root.
- Real snapshot: `pudding-20260917` root `d8ec110f...` (157,874 files) opens and
  verifies in 0.79-0.86 s; `cas_read` returns `loadngo/proactor/src/lib.rs` verified.
- Real tokenizer: a two-call reply parses to the exact ids and arguments.
- Live chat through the launcher: asked for the first lines of `launch-kimi-k3.sh`, Kimi
  called `fs_read {"path": "launch-kimi-k3.sh", "line_start": 1, "line_count": 5}` and
  answered "zsh ... the shebang line `#!/usr/bin/env zsh`". Asked about the snapshot,
  she called `cas_list {"path": ""}` and named real top-level repositories (loadngo,
  loadngo-cpp, kimi-k3-in-c, qcoin, sng-rusty, ...); her one-line descriptions of them
  were guesses, not read.
- Cost: the replies took 104-570 s. The 5 KB listing added ~2,400 prompt tokens at about
  0.2 s each (listings have since been shortened), and the tool declarations cost ~800
  prompt tokens at the start of each conversation.

## Keeping the snapshot current

The model is only as current as its snapshot. Plan: a scoped "workspace code" archive
(the repositories, excluding `target/`, `.git/`, model checkpoints and other large
binaries) ingested with the existing `archive_cas_ingest` into a CAS root on Jarraya,
then signed with `archive_cas_sign`. Unchanged files deduplicate to existing objects;
changed files add new ones. A `/snapshot` chat command could run that ingest on request
(never automatically), and signing stays Jay's action since it uses his key. Ingest time
for the code-only scope is to be measured.

## The speed caveat

File contents become prompt tokens: a 16 KiB read is roughly 4-5 thousand tokens
(*estimate*, at 3.5-4 bytes per token of code; not measured with this tokenizer). At
today's Kimi Linear prompt speed that is minutes before the model answers, so the tools
are usable in earnest only after faster prompt processing (`METAL_COMPUTE_PLAN.md` M1-M3).
Building the tools first is still worthwhile: the loop, bounds and provenance can be
tested with small reads now.

## Still open

- **Speed.** Prompt processing (~0.2 s per token measured here) makes file-heavy questions
  take minutes; that is `METAL_COMPUTE_PLAN.md`'s work, plus caching the tool-declaration
  prefix across conversations.
- **A current snapshot.** `pudding-20260917` predates this week; a code-only snapshot on
  Jarraya, signed by Jay, would let the CAS tools see current code.
- K3's own tool format (XTML, `encoding_k3.py`); K3 chat has no tools yet.
- Proposals as new CAS objects that Jay signs, if writing is ever wanted.
