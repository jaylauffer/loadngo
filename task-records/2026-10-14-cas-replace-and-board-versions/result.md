# Result: a replace is one changed version; the board and Task records in the pudding CAS

Worker: claude-code-macmini (Claude Code, Opus 5.5, in Jay's session). Assignment
15428644392357762276; intent and check in loadngo fff8d820.

commit: fb54b9f5f981f2b7840509d6abf0876369ba950e
test: replacing_entries_records_changed_and_created_in_one_version
manifest: pudding-20260917-73a64d815141799f7e5e8424406ffee475d6e979045e32f97daa561e90f2ca1f.json
board-copy: /Users/jay/pudding/loadngo/task-records/2026-10-14-cas-replace-and-board-versions/AGENT-BOARD.md

## What changed

- data::archive_cas: `with_entries_replaced` (one version: `changed` for replaced files,
  `created` for new paths; same bytes left alone; nothing removed; a file over a
  directory refused); versions may carry several records. `archive_cas_add --replace`.
  docs/ARCHIVE_CAS.md.
- pudding-20260917 (Zhoenus II), two versions above 6a5b6290, both signed by jay-macmini
  (Dilithium2) and verified (`archive_cas_verify`: 7,459 unique objects, 25.5 GB,
  complete):
  - 10c3f439: `changed` AGENT-BOARD.md (the board as condensed 2026-10-14, a copy beside
    this record); `created` agent-board-archive/ with its three files.
  - 73a64d81: `created` loadngo/task-records/ (85 entries: every Task record of the day).

## How it was checked

- Test above, 3 mutations of the replace logic caught; fmt, strict clippy, data tests.
- CI 38487453239 for fb54b9f5 passed on all four platforms before hand-over.
- This check was not run before hand-over: the submitter's run is its first.

## Not done

- The board changes after this snapshot (claims, handoffs); the next version records the
  next change. Making that routine (each handoff, each closed Task) is not automated.
- Deltas between versions (zstd patch-from or bsdiff) and archive_cas_reconcile: later.
