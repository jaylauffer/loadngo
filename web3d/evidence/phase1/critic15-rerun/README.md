# The fifteenth critic's findings, answered: the sixteenth round (2026-11-04)

The sixteenth builder round, on loadngo `web3d-phase1` from `226b7c92` (the fifteenth critic judged
that commit; its own evidence is `../critic15/`, untouched). Chrome 157.0.8103.38 headless
(`channel: 'chrome'`, ANGLE over Metal, Apple M4 Pro); rustc 1.99.0, wasm-opt 133. The Mac's GPU was
claimed for every run.

The page contract (WEB3D.md) is unchanged. The fifteenth critic judged inside it, as its brief
asked, and found one fault in web3d's behaviour on a cooperative page (F1), two misstatements in the
contract (F2) and a miscount (F3).

## Each finding

| Finding | What changed | Evidence |
| --- | --- | --- |
| F1, a renderer made in a page's restored listener ahead of web3d's loses its hooks | The restored listener finishes the current renderer only if a loss finished it (`_shim.lost()`), the critic's trial verbatim. A renderer made since the loss was counted, by a page listener during the restore's dispatch, took the restored context: it stays current, and its hooks are for its own later losses. The reason given for `take()`'s line is restated: the context reads restored during the dispatch, so a renderer still current at a create is either one whose restore is being dispatched now (web3d's listener calls its hook later in the dispatch) or one whose restore was kept from web3d (outside the contract). The `controllerFor` comment, the `onContextRestored` type doc and WEB3D.md's "What the controller does" say so | Probe LR's new case, the first renderer disposed in that listener or not: at the first restore the hooks read `r1 lost`, `r2 created`, `r1 restored`; at r2's own loss `r2 lost`, `r2 restored`; then a new renderer draws. `control/run.sh`: with `226b7c92`'s `web3d.js` the rendered check fails on LR alone, 79 of 80, where r2 gets a spurious `r2 restored` and nothing at its own loss (`control/at-226b7c92/`). The critic's `s1-restored-create.html`, rerun: below |
| F2(a), the built-ins "read at each call" | The contract no longer reads as a complete list: "among them", with `URL`, `Set`, `WeakRef`, `Error`, `TextDecoder`, the typed-array constructors, the statics of `Array`, `ArrayBuffer` and `Math`, and the methods and iterators of web3d's maps, sets and arrays added; `crypto.getRandomValues` moved to load, where it is called once. `Map` is not listed as a constructor: web3d makes its maps at load | WEB3D.md, "The page contract", item 2; each name checked against `js/web3d.js` and `js/gl.js` |
| F2(b), a worker's `closed` | Item 2 and "One realm drives a canvas" name both of the window's getters for a worker, and what refuses: a `top` getter that returns null, a `closed` one that returns true, or either that throws. The comment in `js/web3d.js` says so too | the critic's `results/s2-worker-closed.json`; `ownRealm` |
| F3, 61 files | 60, in WEB3D.md and `../critic14-rerun/README.md` | `../critic14-rerun/booleans.txt` |
| Notes | `frame()`'s docs (WEB3D.md's API table, `js/web3d.js`, `js/web3d.d.ts`) say that a lost renderer returns false before the place checks, so a closed window's lost renderer returns false rather than throwing (corrected in the seventeenth round: this said "before any other check", but a disposed renderer throws first, the sixteenth critic's F3); `../critic14-rerun/README.md` says `onLost` is cleared just before its call | |

## Gates

`../gates.sh 5666 5667`, run through `gates-contention/run.sh`: **every gate held** (`../gates.txt`).

- fmt; clippy as CI runs it and for the crate on wasm32, aarch64 Linux, Windows MSVC and the host;
  75 tests, debug and release;
- three builds byte-identical: wasm `4d6594b5…` (unchanged), `gl.js` `6542cb39…`, `web3d.js`
  `cced8579…`, `web3d.d.ts` `e1b9e4f3…`; no local path in `pkg/`;
- `three-reference.mjs --check`: 20 lines verbatim; `ci-step.rb` on the real workflow: exit 0;
- the rendered check: 80 of 80 probes (LR with its new case), no `INVALID_OPERATION`, 2 pending
  errors raised on purpose, no context lost to Chrome's limit; steady allocation 0 sampled bytes a
  frame, control 89.0; the blind run exited 3, its control 0.5.

| Canvas | GPU ms mean | p50 | p95 | CPU ms p50 (p95) |
| --- | --- | --- | --- | --- |
| 1024x576, MSAA 4x | 0.88 | 0.868 | 0.96 | 0.21 (0.285) |
| 1366x768, MSAA 4x | 1.087 | 1.053 | 1.219 | 0.2 (0.265) |
| 1024x576, no MSAA | 0.653 | 0.589 | 0.917 | 0.185 (0.345) |

Only the desktop Chrome window ran beside the gates (`gates-contention/contention-during.txt`). The
medians are within 0.005 ms of WEB3D.md's quiet table; the means and p95s are above both it and the
fifteenth round's run, with the same wasm, so this run is not the capacity figure.

## Every critic's probes, rerun

`rerun-critics.sh 5668` (`rerun-critics.log`, outputs in `rerun/` and `results/`): **67 runs, each
exiting 0**: every earlier critic's scripts and pages, the twelfth, fourteenth and fifteenth rounds'
pages, and the fifteenth critic's three.

`booleans.sh` compares every boolean leaf with the fifteenth critic's own rerun and pages
(`booleans.txt`): **0 of 1,396 booleans differ, in all 64 files.** Its pages record their logs as
strings, so those were compared too (every non-number leaf):

- `s1-restored-create`, each of its three failing rows (a listener on the canvas added before
  web3d's, disposing then creating or creating without disposing; and a capturing one on the
  window): at the first restore r2's spurious `onContextRestored` is gone, and at r2's own loss and
  restore it now gets `onContextLost` and then `onContextRestored`. F1's fix. Its controls read as
  before.
- `s2-worker-closed`: no string differs (F2(b) was the docs').
- `s3-builtins-per-call`: only a handle's random serial in a refusal's message.

## Files

- `control/run.sh` (a copy of the critic's) and `control/at-226b7c92/`: the rendered check with
  `226b7c92`'s `web3d.js`.
- `tools/run.mjs`, `tools/no-devtools.mjs` and `run10-copy.mjs`: the drivers, copied to write here.
- `rerun-critics.sh` (every critic's scripts and pages, the fifteenth round's `r15.html` and the
  fifteenth critic's three pages among them; `rerun/`, `results/rerun-*.json`, `rerun-critics.log`)
  and `booleans.sh` (`booleans.txt`).
- `gates-contention/`: the gates' wrapper and its record.
