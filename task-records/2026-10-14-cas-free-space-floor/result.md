# Result: Archive CAS writes keep a free-space floor

Worker: claude-code-macmini (Claude Code, Opus 5.5, in Jay's session). Intent and check in
loadngo fc44b4c2.

commit: b8e5d387f5d635a73a6cf06420b95bcef1921f70
test: a_write_below_the_free_space_floor_is_refused_and_leaves_nothing_written

## What changed

- `data::disk` (new): free space on a path's volume (`statvfs`; `GetDiskFreeSpaceExW` on
  Windows), the default floor (the larger of 10 GiB and 5% of the volume), the check and
  its message (free space, volume size, floor).
- `ArchiveCasStorage`: ingest, add, compression and restore check before writing and
  refuse below the floor, writing nothing; a restore checks its destination's volume.
  `set_min_free`; `--min-free-gib` on archive_cas_ingest, archive_cas_add and
  archive_cas_restore; `LOADNGO_CAS_MIN_FREE_GIB` replaces the default (CI sets 0, so the
  test suites do not depend on a hosted runner's disk).
- docs/ARCHIVE_CAS.md "Free space"; .github/workflows/ci.yml (the variable).

## How it was checked

- The test above covers all four write paths (refused, nothing left; allowed at a floor of
  0); disk tests for the floor, the message and a path not yet made. 5 mutations caught.
- fmt; strict clippy for data, inference and host-desktop on macOS; data clippy and the
  floor tests on dolores (Linux).
- Not run before hand-over: this check (the submitter's run is its first).
- CI 38496879520 (at 63850d2c, which contains b8e5d387) passed on all four platforms; the run at
  b8e5d387 itself was cancelled by that newer push after Linux and Windows had passed.
- The first Task run was stopped at a usage limit before hand-over (its logs:
  first-run-*.log beside this run's); this is a second request against the same intent.

## Disk at hand-over

- Mac internal disk 174 GiB free (81%) after Jay's cleanup this morning (was 56 GiB, 94%);
  Zhoenus II 909 GiB free; dolores 108 GiB free.
