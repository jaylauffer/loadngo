# CI runners

loadngo's CI builds loadngo alone. Until 2026-10-06 it also checked out QCoin
beside it, because loadngo's workspace had a path dependency on
`../qcoin/qcoin-types` for Task rewards; that dependency is gone (see
`TASK_REWARD_FLOW.md`), and with it `QCOIN_REV`.

## Runner

Since 2026-10-06 every platform runs on GitHub-hosted runners
(`ubuntu-24.04-arm`, `ubuntu-latest`, `macos-latest`, `windows-latest`):
loadngo is public, so the minutes are free. Each job checks out loadngo with
`actions/checkout` and caches the build with `Swatinem/rust-cache`. Windows moved first: it had run on the lab's
self-hosted `acerj` (`build-windows-x64`, provisioned by
`~/pudding/provision-windows-runner.ps1`), which was often switched off, so
Windows jobs sat queued until it woke. Linux (dolores, agnes) and macOS (the
Mac mini, building on Jarraya) followed the same day, which also means no pull
request from a fork runs on lab hardware. The hosted Windows image has what
acerj was provisioned with: the VS C++ build tools, Git and rustup. Linux
installs `pkg-config`, `libasound2-dev` and `libwayland-dev`, which alsa-sys
and wayland-sys look for at build time, and `libegl-dev` and `libgles-dev`,
which host-desktop's harness binaries link.

## History: the QCoin sibling

In [run 35762838840](https://github.com/jaylauffer/loadngo/actions/runs/35762838840)
acerj fetched loadngo `af005bf6`, passed formatting, then failed dependency
resolution under `--locked`: its QCoin clone was stale, because nothing
advanced it. `QCOIN_REV` was added to the Windows job for that (first pin
`6579c0a5`, which has the move from `qcoin-crypto` to `loadngo-pq-crypto`).
The Linux and macOS jobs had the same gap (their QCoin clones were from
09-17) until loadngo `75a48796` pinned them too.
