# The fourteenth critic's findings, answered: the fifteenth round (2026-11-04)

The fifteenth builder round, on loadngo `web3d-phase1` from `9a4ab9c7` (the fourteenth critic judged
that commit; its own evidence is `../critic14/`, untouched). A first session made part of the round
and stopped (`e6a78183`, WIP, no gate run); a second finished it. Chrome 157.0.8103.38 headless
(`channel: 'chrome'`, ANGLE over Metal, Apple M4 Pro); rustc 1.99.0, wasm-opt 133. The Mac's GPU was
claimed for every run; during the gates only a desktop Chrome window shared it
(`gates-contention/contention-during.txt`, with a note on reading it).

## What the round decided

The supported model is written down as **the page contract** (WEB3D.md, "The page contract"): one
copy of web3d, in the canvas's own realm, drives a canvas, on a page that does not keep a WebGL
context event from web3d's listener (stopping it, or reopening the document from a listener ahead
of web3d's), does not shadow or patch a built-in web3d reads, and does not move a driven canvas
element to another document. web3d carries no code for pages outside it; WEB3D.md states what each
case costs, with where it was measured. Fourteen rounds each found one more way a hostile page can
break recovery; the contract is where that stops, and it is set by how web3d is used.

## Each finding

| Finding | What changed | Evidence |
| --- | --- | --- |
| F1, a page's `function closed() {}` turns the removed-frame check off | `ownRealm()` reads two built-in getters of the window, taken at load: `top`, which no page code can replace (a page's `function top() {}` throws) and which reads null from a removal's own task on; and `closed`, which reads true from `close()` on, so a closing window is refused at once, as before. A page's `function closed() {}` leaves `CLOSED` undefined, and `top` alone still sees a removal. The critic's tried line (`GLOBAL.top !== null`) was not taken as it stood: in a worker, which has no built-in `top`, a page's `globalThis.top = null` would turn it false and refuse every canvas; the built-in getter is undefined there | `results/r15.json` T1 (both frames: `top` null in the removal's task, every canvas then taken by this window's copy), T2 (a plain popup refused from `close()` on; one that declared `function closed` refused once `top` reads null, 3 ms later; its `closed` key is missing from the JSON because the value is the page's function), T3 (a worker: a data `top` harmless; a page-defined `top` getter refuses every canvas, now in the contract); probe NV's new case (`demo/probe-frame-closed.html`) in the rendered check |
| F2(a), a missed restore's hooks | `take()` moves a still-current renderer's `onContextRestored` to the waiting set before the new renderer takes the canvas (the critic's tried fix, verbatim), and the lost listener clears `onLost` just before calling it, so a renderer hears one loss | `results/r15.json` T5: each of the critic's four R4 variants gets each renderer's `onContextLost` once and the first renderer's `onContextRestored` once at the next restore |
| F2(b), the reasons for leaving the cases open | WEB3D.md's paragraph and `../critic13-rerun/README.md` no longer give the disproved reasons. WEB3D.md, "Why the last two cases stay open", says what the critic showed (both cases could be narrowed), what no fix covers (a canvas in a shadow root; a listener that stops the event), and that they stay open because they are outside the contract. The third way a listener gets ahead (after a failed create took web3d's off) is listed | WEB3D.md, "The loss controller"; `../critic13-rerun/README.md`, marked corrected |
| F3, stopping propagation | The contract's first item names each place, measured. A capturing listener on the canvas itself runs before web3d's whatever the order, so its `stopPropagation()` keeps the event from web3d's listener too; a non-capturing listener ahead of web3d's may call `stopPropagation()` or set `cancelBubble` without effect on web3d, which is on the same node. The comments in `js/web3d.js` and `js/web3d.d.ts` say "every lost event that reaches its listener" and point to the contract | `results/r15.json` T4: 13 places for each of the two events, each with its outcome; the controls recover |
| F4, the browser version | `../critic13-rerun/README.md` names Chrome 157.0.8103.38, marked corrected. WEB3D.md's Results says the browser changed from 156 to 157 since the quiet table, and that the near-quiet runs on 157 (the critic's and this round's) agree with it | WEB3D.md, Results |
| Note: the CI-step gate | `../ci-step.rb` now refuses (exit 2, with its reason; a Ruby error exits 1) an `env` name outside `CARGO_TERM_COLOR` and `LOADNGO_CAS_MIN_FREE_GIB` at any level, a `run` naming `GITHUB_ENV` or `GITHUB_PATH`, a job `if`, job keys beyond today's, another `runs-on`, actions or inputs beyond today's two, a step outside the checkout, filters other than `paths-ignore`, and a `paths-ignore` pattern that matches what the step reads. WEB3D.md states what it checks and what it does not | `yaml-gate/run.sh` and `results.txt`: 53 of 53 rows hold, among them the critic's seven copies, each refused with its expected reason, and two broken gate copies judged not to refuse; `yaml-gate/critic14-run.sh`, the critic's own probe, copied: all seven refused |
| Note: the confidentiality scan | Widened in the game's repository, where its pattern lives | that repository's `evidence/web3d/round15/` |
| Polish | A closing popup's `frame()` names a closed window; the `createRenderer` comment in `js/web3d.d.ts` is wrapped | `js/web3d.js` `NAVIGATED` |

## Gates

`../gates.sh 5662 5663`, run through `gates-contention/run.sh`: **every gate held** (`../gates.txt`).

- fmt; clippy as CI runs it and for the crate on wasm32, aarch64 Linux, Windows MSVC and the host;
  75 tests, debug and release;
- three builds byte-identical: wasm `4d6594b5…` (unchanged, so the GPU's work is too), `gl.js`
  `6542cb39…`, `web3d.js` `bdbf9ec6…`, `web3d.d.ts` `2f7be9a7…`; no local path in `pkg/`;
- `three-reference.mjs --check`: 20 lines verbatim; `ci-step.rb` on the real workflow: exit 0;
- the rendered check: 80 of 80 probes, no `INVALID_OPERATION`, 2 pending errors raised on purpose,
  no context lost to Chrome's limit; steady allocation 0.09 sampled bytes a frame (56 bytes, the
  check's own call; none in `pkg/`), control 88.9; the blind run exited 3, its control 0.5.

| Canvas | GPU ms mean | p50 | p95 | CPU ms p50 (p95) |
| --- | --- | --- | --- | --- |
| 1024x576, MSAA 4x | 0.855 | 0.847 | 0.884 | 0.05 (0.245) |
| 1366x768, MSAA 4x | 1.056 | 1.048 | 1.116 | 0.175 (0.255) |
| 1024x576, no MSAA | 0.566 | 0.565 | 0.579 | 0.045 (0.055) |

The medians are within 0.02 ms of WEB3D.md's quiet table (Chrome 156), the p95s within 0.05 ms.

## Every critic's probes, rerun

`rerun-critics.sh 5664` (`rerun-critics.log`, outputs in `rerun/` and `results/`): **64 runs, each
exiting 0**: every earlier critic's scripts and pages, the twelfth and fourteenth rounds' pages, the
fourteenth critic's seven (`c2` also with no DevTools client, which keeps prerendering on), and this
round's `r15.html`.

`booleans.sh` compares every boolean leaf with the fourteenth critic's own rerun and pages
(`booleans.txt`): **0 differ in all 60 files.** The critic's pages record some outcomes as strings,
so those were compared too (every non-number leaf, `c1-closed` and `c3-reopen`):

- `c1-closed`, A1 (a frame that declared `function closed() {}` before web3d loaded) and A1d (one
  that redefined `closed`, outside the contract): the removed copy's create is now refused ("navigated
  away, ... removed or ... closed") where it was "still pending after 500 ms", and this window's copy
  creates and draws where it was refused "already has a live renderer". F1's fix.
- `c3-reopen`, R4: "no dispose, then create" now reads "first restored" then "second restored" at
  the second restore, and "no dispose, no create" reads "first lost" once and "first restored" at the
  second restore. F2(a)'s fix.
- Every other string that differs is the refusal's message, which now names a closed window.
  `G_popup`'s `frameRightAfterClose` is still refused at once.

`c5-top-fix` and `c6-r4-fix` load the critic's own trial packages, so they reproduce its trials, not
this round's code; `r15.html` T5 runs `c6`'s four variants against this round's code.

## Files

- `site/`: this round's page `r15.html`, with `frame.html`, `frame-fnclosed.html`, `popup.html`,
  `popup-fnclosed.html` and `worker-top.js`. `results/r15.json` is its result.
- `tools/run.mjs` and `tools/no-devtools.mjs`: the fourteenth critic's drivers, copied to write
  here. `run10-copy.mjs`: the tenth critic's driver, likewise.
- `rerun-critics.sh` (every critic's scripts and pages, this round's page among them; `rerun/`,
  `results/rerun-*.json`, `rerun-critics.log`) and `booleans.sh` (`booleans.txt`).
- `gates-contention/`: `run.sh` and its record.
- `yaml-gate/`: the CI-step gate's controls.

Run the page: `PLAYWRIGHT_CORE=.../playwright-core/index.mjs node tools/run.mjs <port> r15.html --timeout 300000`.
