# Windows CI and the QCoin sibling

loadngo has a path dependency on `../qcoin/qcoin-types`, so every CI job
builds with a QCoin checkout beside loadngo, at the workflow-level `QCOIN_REV`.
Cargo.lock records QCoin's dependencies, so with `--locked` a pin and a lock
that disagree fail dependency resolution. When Cargo.lock is refreshed for a
newer QCoin, move `QCOIN_REV` to that commit in the same loadngo commit. Do
not substitute a floating branch or regenerate Cargo.lock in CI.

## Runner

Since 2026-10-06 the Windows job runs on GitHub-hosted `windows-latest`
(loadngo is public, so the minutes are free). It checks out loadngo and QCoin
side by side with `actions/checkout` and caches the build with
`Swatinem/rust-cache`. Before that it ran on the lab's self-hosted `acerj`
(`build-windows-x64`, provisioned by `~/pudding/provision-windows-runner.ps1`),
which was often switched off, so Windows jobs sat queued until it woke.
The hosted image has what acerj was provisioned with: the VS C++ build tools,
Git and rustup.

Linux and macOS still run on self-hosted runners with persistent clones under
`ci/pudding`; their QCoin clone is synced to `QCOIN_REV` by a step that
refuses to overwrite local changes.

## History

In [run 35762838840](https://github.com/jaylauffer/loadngo/actions/runs/35762838840)
acerj fetched loadngo `af005bf6`, passed formatting, then failed dependency
resolution under `--locked`: its QCoin clone was stale, because nothing
advanced it. `QCOIN_REV` was added to the Windows job for that (first pin
`6579c0a5`, which has the move from `qcoin-crypto` to `loadngo-pq-crypto`).
The Linux and macOS jobs had the same gap (their QCoin clones were from
09-17) until loadngo `75a48796` pinned them too.
