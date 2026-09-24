# Local models read through the loadngo CAS: plan

Status: plan, 2026-09-24 (Claude Code). Jay: "We should enable Kimi to leverage the loadngo
CAS, that should be her default capability." Nothing here is built yet.

## The idea

A local model (Kimi Linear today, K3 later) gets read access to files, but never to the
live filesystem. Its only view is a **signed Archive CAS snapshot**: a manifest mapping
paths to BLAKE3 object hashes, whose root hash carries Jay's Dilithium signature. That
gives, for free, what an agent's file access otherwise lacks:

- **Nothing to damage.** Reads come from immutable objects; there is no write path and
  no path outside the manifest.
- **Every quote is checkable.** Each tool result names the path, the object hash and the
  signed root, so a claim like "`foo.rs` line 40 does X" can be verified by anyone holding
  the public key, the same standard the workspace applies to agents' status claims
  (`COLLABORATION.md` rule 5).
- **A known state.** The model answers about one identified snapshot, not whatever the
  checkout happens to hold mid-edit.

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

## Tools the model gets

Declared through the model's own tool format: Kimi Linear's `chat_template.jinja`
has a `tool_declare` system message, `<|tool_calls_section_begin|>` / `<|tool_call_begin|>`
calls and `## Return of <id>` results; its tokenizer has the five `<|tool_*|>` control
tokens as single ids (163595-163599). All read-only, all bounded:

| Tool | Arguments | Returns |
|---|---|---|
| `cas_list` | `path` (directory) | children with kind, size, object hash; at most 200 entries per call |
| `cas_find` | `pattern` (path glob) | matching paths from the manifest; at most 100 |
| `cas_read` | `path`, optional `line_start`, `line_count` | UTF-8 text, at most 16 KiB per call; binaries report size and hash only |
| `cas_grep` | `pattern` (literal), optional `path_prefix` | matching lines with path:line, scanning at most 32 MiB of text per call |

Every result starts with `root <hex> (signed by jay-macmini)`, `path` and `object <hex>`.
Each blob is BLAKE3-verified before any byte of it reaches the model.

## Where the code goes

- **loadngo (BSD-3), `data`:** a read-only `ArchiveView`: open a root, check the
  signature against a trusted key, build a path index once, and serve list/find/read/grep
  with the bounds above. No model knowledge.
- **loadngo, `inference`:** a model-independent tool loop in the chat session: detect a
  completed tool call in the generated tokens, execute it (bounded, cancellable), append
  the result turn, and continue generation. Undo/reset/continue keep their current
  meaning.
- **kimi-k3-in-rust (Apache), `chat.rs`:** the Kimi Linear tool-call encoding and parsing
  (the `<|tool_*|>` tokens), and the tool declaration in the system turn. K3's XTML tool
  format later, from `encoding_k3.py`.
- **Launcher:** `--cas-root` (default: the newest snapshot that verifies against the
  trusted key); chat refuses to start tools on an unsigned or mismatched root and says so.

## Default behaviour

- Tools on by default in chat when a verified snapshot is found; `--no-tools` turns them
  off. Without a verified snapshot, chat still works, without tools, and prints why.
- Tool calls execute automatically (they are side-effect free) and are echoed in the
  terminal as they happen: `[cas_read kimi-k3-in-rust/crates/kimi-k3-core/src/linear.rs
  1-80 -> object 3fa1...]`.
- No writes. A later step could let the model *propose* changes as new CAS objects and a
  candidate manifest that only Jay signs, which fits `PUDDING_CAS_PQ_MODEL.md`, but that is
  out of scope here.

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

## Phases and gates

| Phase | Work | Gate |
|---|---|---|
| C0 | `ArchiveView` in `data` with list/find/read/grep and signature checking | unit tests on a scratch signed root: tampered manifest, wrong key and tampered blob are all refused; bounds enforced |
| C1 | Tool loop in `inference`; Kimi Linear tool encoding in kimi | fake-model tests of call parsing, execution, cancellation and undo; the real tokenizer's tool tokens are single ids |
| C2 | Live: Kimi Linear answers a question that needs a file, from the signed snapshot | transcript recorded with each result's path, object and root; the answer checked against the file |
| C3 | Scoped code snapshot on Jarraya, signed by Jay; measured ingest time | root verifies; model can read this week's code |
