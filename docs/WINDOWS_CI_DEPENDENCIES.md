# Windows CI sibling dependency

The Windows job builds `C:\pudding\loadngo`, which has a path dependency on
`C:\pudding\qcoin\qcoin-types`. Updating Loadngo alone does not update that
sibling's manifests. The provisioning script creates missing clones but does
not advance existing QCoin checkouts.

The workflow now pins QCoin with `QCOIN_REV` and synchronizes the dedicated
CI sibling before Cargo runs. Revision `6579c0a54635ef913806922a88049a6b5e68e840`
includes the move from `qcoin-crypto` to `loadngo-pq-crypto` expected by the
current Loadngo lockfile. The step logs the old and new QCoin revisions, fetches
only from the public GitHub QCoin repository, and checks out the exact revision
in detached-HEAD mode. It requires the existing clone and refuses tracked or
untracked local changes; it does not reset or clean the sibling.

Keep `--locked` on Clippy and tests. When changing this path dependency, update
the pin deliberately and verify the matching manifests/lockfile together.
Do not substitute a floating branch or silently regenerate Cargo.lock in CI.
Linux and macOS job behavior is unchanged by this Windows-specific repair.

## Evidence and remaining validation

In [run 35762838840](https://github.com/jaylauffer/loadngo/actions/runs/35762838840),
Acerj fetched Loadngo `af005bf6`, passed formatting, then failed dependency
resolution under `--locked` before linting. Linux and macOS passed. A stale
Windows sibling was a hypothesis because Acerj has no SSH access; the new
step makes the dependency revision reproducible and exposes it in job logs.
Only a fresh Windows run can establish whether it was the entire cause or
whether Windows-specific resolution/build issues remain.

If synchronization refuses local changes, inspect the printed status and
preserve those files before retrying. Do not add `reset --hard` or `clean -fd`
as a workaround. If the pinned sibling still produces a lockfile error, inspect
resolution in a disposable copy with the runner's Cargo version and review the
minimal lockfile delta. A rerun of an old workflow does not pick up this step;
use a run triggered by the commit containing the repair.
