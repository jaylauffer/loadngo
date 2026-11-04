# The sixteenth critic's findings, answered: the seventeenth round (2026-11-04)

The seventeenth builder round, on loadngo `web3d-phase1` from `dfeb1b91` (the sixteenth critic judged
that commit; its own evidence is `../critic16/`, untouched). Chrome 157.0.8103.38 headless
(`channel: 'chrome'`, ANGLE over Metal, Apple M4 Pro); rustc 1.99.0, wasm-opt 133. The Mac's GPU was
claimed for every run. The page contract is unchanged in scope; item 1 now also states what follows
when a page stops a lost event and cancels it itself.

## Each finding

| Finding | What changed | Evidence |
| --- | --- | --- |
| F1, a loss the page makes during the restore's dispatch spends the next renderer's hooks | The critic's third trial, with this round's comments. The restored listener does nothing when the context reads lost again (page code lost it in a listener of its own or a create's `then`, before web3d's listener ran): every hook waits for the restore after that loss, and the live renderer stays current to hear it. A renderer is finished at a restore only when a loss was counted after it took the context (the count it took the context at is kept on the controller), not when it reads lost, which a loss still to be dispatched also makes true | Probe LR's newest case, the first renderer disposed in the page's listener or not: at the re-loss the hooks read `r1 lost`, `r2 created`, `r2 lost`; at the restore after it `r1 restored live`, `r2 restored live`; then a new renderer draws. `control/run.sh`: with `dfeb1b91`'s `web3d.js` the rendered check fails on LR alone, 79 of 80, where both restore hooks come while the context reads lost and nothing at r2's own loss (`control/at-dfeb1b91/`). The critic's `g1-loss-in-dispatch.html`, rerun: below |
| F2, item 2's "every other built-in ... at each call" | Item 2 now separates built-ins web3d uses only while it loads (among them `Map`, `WeakMap`, `WeakSet`, `Symbol.for`, `Object.freeze`, `Object.getPrototypeOf`, `Object.getOwnPropertyDescriptor`, and `crypto.getRandomValues`, called once) from those it reads at each call. `Object`'s statics are no longer in the per-call list; `Reflect`'s, `Number`'s, `Array`'s and `ArrayBuffer`'s, each used inside a function, stay | the critic's `results/b1-builtins-at-load.json`; WEB3D.md, "The page contract" |
| F3, "(before any other check)" | `frame()`'s docs say that a lost renderer returns false after the disposed and re-entrancy checks, which throw, and before the place checks: WEB3D.md's API table, `js/web3d.js`, `js/web3d.d.ts`, and `../critic15-rerun/README.md`, marked corrected | the critic's `results/o1-frame-order.json` |
| Notes | Item 1's "If it does" now says what happens when the page stops a lost event and cancels it itself (the context comes back; web3d counted no loss, so no hook comes; a renderer that read nothing meanwhile reads live, draws with dead objects and keeps the canvas; one that read the loss reads lost, and a new create takes the canvas; the critic's `results/n1-stopped-and-cancelled.json`). Item 2's worker sentence says a `closed` getter that returns any truthy value. `gates.sh`'s header points to the newest round's rerun script | |

## Gates

`../gates.sh 5670 5671`, run through `gates-contention/run.sh`: **every gate held** (`../gates.txt`).

- fmt; clippy as CI runs it and for the crate on wasm32, aarch64 Linux, Windows MSVC and the host;
  75 tests, debug and release;
- three builds byte-identical: wasm `4d6594b5…` (unchanged), `gl.js` `6542cb39…`, `web3d.js`
  `2caa6279…`, `web3d.d.ts` `abcb853c…`; no local path in `pkg/`;
- `three-reference.mjs --check`: 20 lines verbatim; `ci-step.rb` on the real workflow: exit 0;
- the rendered check: 80 of 80 probes (LR with both new cases), no `INVALID_OPERATION`, 2 pending
  errors raised on purpose, no context lost to Chrome's limit; steady allocation 0.16 sampled bytes a
  frame (96 bytes, all the check's own measuring call; none in `pkg/`), control 90.0; the blind run
  exited 3, its control 0.4.

| Canvas | GPU ms mean | p50 | p95 | CPU ms p50 (p95) |
| --- | --- | --- | --- | --- |
| 1024x576, MSAA 4x | 0.878 | 0.869 | 0.909 | 0.185 (0.25) |
| 1366x768, MSAA 4x | 1.053 | 1.048 | 1.076 | 0.185 (0.26) |
| 1024x576, no MSAA | 0.603 | 0.587 | 0.727 | 0.155 (0.255) |

Only the desktop Chrome window ran beside the gates (`gates-contention/contention-during.txt`). The
medians are within 0.006 ms of WEB3D.md's quiet table, the p95s within 0.12 ms.

## Every critic's probes, rerun

`rerun-critics.sh 5672` (`rerun-critics.log`, outputs in `rerun/` and `results/`): **71 runs, each
exiting 0**: every earlier critic's scripts and pages, the rounds' own pages, and the sixteenth
critic's four.

`booleans.sh` compares every boolean leaf with the sixteenth critic's own rerun and pages
(`booleans.txt`): of 1,420 booleans in 68 files, **13 differ, all in its F1 page and all from false
to true**: in each of its five failing rows, r2's `onContextRestored` now comes only at its own
restore, r2 hears its own loss once, and the row reads `asDocumented`. Every other file: 0 differ.
Strings, compared too (every non-number leaf):

- `g1-loss-in-dispatch`: in each failing row the restore hooks no longer come while the context
  reads lost; r2's `onContextLost` comes at the re-loss, then `r1 onContextRestored` and
  `r2 onContextRestored` at the restore after it, the context live. F1's fix.
- `n1-stopped-and-cancelled` (outside the contract): in the variant that called `frame()` while the
  context was lost, the renderer's `onContextRestored` no longer comes, since web3d counted no loss;
  the renderer reads lost and the next create draws, as before. Item 1 now states both variants.
- `b1-builtins-at-load` and `o1-frame-order`: only ports, a handle's random serial and stack lines.

## Files

- `control/run.sh` (a copy of the sixteenth round's) and `control/at-dfeb1b91/`: the rendered check
  with `dfeb1b91`'s `web3d.js`. Its first run, before the probe was fixed, caught nothing of the
  second loss: the probe began listening for it only after the restore, when it had been
  dispatched already. The probe now listens from the page's own handler, where the loss is made;
  `control/run.log` is the run after that fix.
- `tools/run.mjs`, `tools/no-devtools.mjs` and `run10-copy.mjs`: the drivers, copied to write here.
- `rerun-critics.sh` (every critic's scripts and pages, the rounds' own pages and the sixteenth
  critic's four among them; `rerun/`, `results/rerun-*.json`, `rerun-critics.log`) and
  `booleans.sh` (`booleans.txt`).
- `gates-contention/`: the gates' wrapper and its record.
