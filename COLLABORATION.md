# Multi-Device Collaboration Protocol

This file defines how two Codex sessions can work safely in `loadngo` without stepping on each other.

## Scope
Applies to:
- `loadngo/`

## Branching Rules
1. Use one active branch per device per repo.
2. Branch naming format:
   - `jay/<device>/<topic>`
   - Examples: `jay/laptop/loadngo-task-ui`, `jay/desktop/network-retry-backoff`
3. Never edit from the same branch on two devices.
4. Keep `main` clean: no direct commits.

## Ownership Rules
1. Before editing, claim files/areas in `WORKLOG.md`.
2. Only one device can own a file subtree at a time.
3. If overlap is needed, coordinate and split by file boundaries.

Suggested `WORKLOG.md` row format:

```md
| UTC time | Repo | Branch | Device | Claimed paths | Status |
|---|---|---|---|---|---|
| 2026-03-08T11:20Z | loadngo | jay/laptop/loadngo-task-ui | laptop | gui/, task/ | in_progress |
```

## Required Git Config (Set Once Per Repo)
Run this once in `loadngo`:

```bash
git config pull.rebase true
git config pull.ff only
git config merge.ff only
```

## Daily Git Loop (Rebase-Only)
Run every 30-60 minutes:

```bash
git fetch origin
git rebase origin/main
cargo check
# run tests if relevant
git push --force-with-lease
```

Rules:
- Rebase only. No merge commits.
- Only update remote history via `git pull --rebase --ff-only` or `git fetch` + `git rebase`.
- Do not run plain `git pull` and do not run `git merge`.
- Use `--force-with-lease` after rebasing published branches.
- If conflicts take more than 10 minutes, stop and split work into smaller branches.

## PR Strategy
1. One logical change per branch.
2. Use stacked PRs when dependent:
   - Base PR-B on PR-A branch.
   - Rebase PR-B after PR-A updates.
3. PR description must include:
   - Why this change exists
   - Files touched
   - Validation commands run

## Quick Start Commands
Create a new branch:

```bash
cd loadngo && git checkout -b jay/<device>/<topic>
```
