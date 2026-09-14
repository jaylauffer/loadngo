# Collaboration

`loadngo` follows the workspace-wide protocol in
[`../COLLABORATION.md`](../COLLABORATION.md), and claims live on the shared
board, [`../AGENT-BOARD.md`](../AGENT-BOARD.md). Both are at the `pudding`
root, next to this repository.

The March 2026 rules that used to be here assumed two sessions on two
machines, one branch per device. Claude Code and Codex now share one
checkout on one machine, so claims, staging, and pushes follow the root
protocol instead.

Still true for this repo:

- History is rebase and fast-forward only; no merge commits.
- Run the CI gates before pushing: `cargo fmt --check` and
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  with CI's `PLATFORM_EXCLUDES` (see `README.md`). `dolores` is the real
  Linux gate.
- `WORKLOG.md` is kept as history; new claims go on the root board.
