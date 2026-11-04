# Critic 15: phase 1 at `226b7c92`, judged (2026-11-04)

Branch `web3d-phase1` at `226b7c92`, working tree clean before this folder. Chrome 157.0.8103.38
headless (`channel: 'chrome'`, ANGLE over Metal, Apple M4 Pro); rustc 1.99.0, wasm-opt 133. Judged
under the scope contract this critic was given, which takes WEB3D.md's page contract as the
supported model: a finding is a misbehaviour on a cooperative page, a false claim, a gate that does
not gate, evidence that does not support what cites it, or a name or home path in a tracked file;
new hostile-page cases are notes.

**Verdict: FAIL.** Every gate passes, every earlier critic's page reruns as the round's own rerun
did (0 booleans and 0 strings differ), and the round's `top` check holds against each removal and
close tried. It fails on:

- F1, a cooperative page that makes its next renderer from a `webglcontextrestored` listener of its
  own that runs before web3d's: the new renderer is handed the restore it was made in, and its own
  later loss is silent. Four claims say otherwise, one of them the reason this round gives for its
  change to `take()`;
- F2, the page contract's item 2 misstates what web3d reads: the list of built-ins read at each call
  leaves out at least six that are, and the worker case names `top` but not `closed`, which does
  the same;
- F3, the round's README and WEB3D.md count 61 result files compared; `booleans.txt` compares 60.

## Findings

### F1. A page that recreates from its own restored listener loses its new renderer's hooks

**The page.** It stops no event, reopens no document, patches no built-in and moves no canvas, so it
is cooperative by the contract's three items. It does the usual WebGL recovery: a
`webglcontextrestored` listener of its own makes the next renderer. Its listener runs before
web3d's when it sits on the canvas and was added before web3d's first create there, or when it is
capturing on the window (both tested here), the document or an ancestor. WEB3D.md 614-617 promises
the live renderer its hook "however the page orders its own listeners".

**What happens.** By the time that listener runs, the context already reads restored (the page's
listener logs `isContextLost()` false; probe LR records the same, `restoredLostAtDispose: false`).
The compiled module is cached and `WebAssembly.instantiate` of a module settles in microtasks, so
the whole `createRenderer` finishes at the microtask checkpoint after the page's listener, before
web3d's restored listener runs. `take()` makes the new renderer `controller.current`. Then web3d's
restored listener takes `current` to be the renderer live at the loss: it calls the new renderer's
`onContextRestored` and sets `current` to null. The new renderer draws, but the controller has
forgotten it. At its own loss, the loss is cancelled and the context comes back, yet neither of its
hooks is called, and `frame()` returns false: the page is never told.

`results/s1-restored-create.json` (each case logs in order; "r2" is the renderer the page makes):

| The page's listener | Log at the first restore | At r2's own loss and restore |
| --- | --- | --- |
| on the canvas, added before web3d's: dispose, then create | `r1 onContextLost`, page listener (context lost: false), `r2 created`, `r1 onContextRestored`, **`r2 onContextRestored`** | **no hook**; restored true; `r2.frame()` false |
| the same, creating without disposing | the same | the same |
| capturing on the window: dispose, then create | the same | the same |
| control: on the canvas, added after web3d's | `r1 onContextLost`, `r1 onContextRestored`, page listener, `r2 created` | `r2 onContextLost`, `r2 onContextRestored` |
| control: create in `onContextRestored`, as WEB3D.md says | the same as the row above | the same as the row above |
| control: dispose only in the listener ahead (probe LR), create in `onContextRestored` | the same as the row above | the same as the row above |

A page that, as WEB3D.md advises, creates again when a renderer's `onContextRestored` comes is
refused at the spurious call: r2 is live ("this canvas already has a live renderer").

**Claims this makes false:**

- WEB3D.md 620-623 (probes LE, ET): a create that takes the restored context "succeeds, and its
  hooks are for later losses". r2 took the restored context; its hooks were spent on the restore
  before it and are not called for its later loss.
- WEB3D.md 609-612, and the `controllerFor` comment (`js/web3d.js` 819-823): on a restored event the
  controller calls `onContextRestored` "for the renderer that was live at the loss, for every
  renderer disposed while the context was lost, and for every create that failed because of the
  loss ... Then no renderer is live until a new one is created". It calls it for a renderer made
  after the loss, and r2 is live afterwards.
- New this round, WEB3D.md 577-580 and the comment in `take()` (`js/web3d.js` 750-753): "A renderer
  still current when the next one is created was finished by a loss whose restore web3d never heard,
  since in Chrome the context reads restored only once its restored event has run". In the second
  row r1 is still current when r2 is created, and web3d hears that restore a moment later. The
  context reads restored during its restored event's dispatch, before web3d's listener (the page's
  log; probe LR). `c7-restore-gap.json`, which the sentence cites, polled in tasks of their own, so it
  could not see inside the dispatch.
- `js/web3d.d.ts` 49-50 (`onContextLost`: "Called when the canvas's context is lost while this
  renderer is the canvas's live one") and 54-56 (`onContextRestored`: "Called once when the context
  is restored after a loss of this renderer's"): r2 is live at its loss and gets neither hook; it
  gets `onContextRestored` for a loss before it existed.

The round's new line in `take()` does not cause this. `results/s1-restored-create-at-9a4ab9c7.json`
runs the page against `9a4ab9c7`'s `web3d.js`: the same rows fail the same way, and there the
create-without-disposing row also loses r1's `onContextRestored`. The new line is what keeps r1's
hook in that row now; its stated reason is the false sentence above.

**Fix, tried here.** In the restored listener, finish `current` only if its renderer was finished by a
loss (`current.renderer._shim.lost()`, which compares the canvas's loss count with the renderer's
own); a renderer made after the loss was counted stays current, so its hooks stay for later losses.
`fix-pkg/web3d.js` changes only those lines. `results/s1-restored-create-fix.json` runs the same
page against it: in every row r1's `onContextRestored` comes once at the first restore and r2's two
hooks come at r2's own loss. With it the whole rendered check exits 0: 80 of 80 probes, steady
allocation 0.09 sampled bytes a frame, control 89.3 (`control/fix-pkg/`, run by `control/run.sh` on
a temporary copy of `demo/` and `pkg/` with only `web3d.js` swapped).

### F2. The page contract's item 2 misstates what web3d reads

Item 2 is defined by "a built-in that web3d reads", and WEB3D.md 143-153 says which those are.

**(a) The list of what web3d reads at each call is incomplete.** WEB3D.md 144-148: "It reads the rest
at each call: the global functions and constructors it calls (`WebAssembly`'s, `fetch`, `Object`'s,
`Reflect`'s and `Number`'s statics, `crypto.getRandomValues`, `reportError`, `setTimeout`,
`performance.now`) and every WebGL2 method `js/gl.js` calls". `site/s3-builtins-per-call.html`
replaces six other globals after web3d has loaded, each with a wrapper that counts its calls and
otherwise behaves as the built-in, then makes ordinary calls. Each wrapper is reached
(`results/s3-builtins-per-call.json`):

| Global | Reached by |
| --- | --- |
| `URL` | `createRenderer` (compile resolves the module's URL) |
| `Set` | the first create on a canvas (its controller), and every restore (`new Set(c.waiting)`) |
| `WeakRef` | the first create on a canvas (`arm`) |
| `TextDecoder` | every create (`createGl()` makes one), and every error from the Rust side |
| `Error` | every refused call |
| `ArrayBuffer.isView` | `compile()` given bytes |

`js/web3d.js` also calls, on one path or another, the typed-array constructors, `Promise.resolve`,
`JSON.stringify` and `String` (in its messages), `Boolean`, the array iterator (`for ... of` over
array literals) and `String.prototype` methods. And `crypto.getRandomValues`, which the list does
name, is called once, when the first copy in a home loads, not at each call. A page that reads the
list to learn which built-ins it may not touch is misled about every one of these.

**(b) The worker case names `top` but not `closed`.** WEB3D.md 150-153: "In a worker, which has no
built-in `top`, the page also does not give the global a `top` getter before web3d loads: web3d would
read it as a window's, and refuse every canvas". web3d takes `closed` the same way
(`getter(globalThis, 'closed')`, `js/web3d.js` 329), and a worker has no built-in `closed` either, so
a worker that gives its global a `closed` accessor is not touching a built-in, and is cooperative by
the contract's letter. `site/s2-worker-closed.html` (`results/s2-worker-closed.json`):

| The worker, before it imports web3d | Its create on the OffscreenCanvas it is sent |
| --- | --- |
| nothing (control) | creates, draws |
| `globalThis.closed = true` (data) | creates, draws |
| a `closed` accessor that returns true | **refused**, "navigated away ... or a window that has been closed" |
| a `closed` accessor that returns false | creates, draws |

So WEB3D.md 317 ("A worker has neither getter and is always its own") and the comment at
`js/web3d.js` 326 ("A worker has neither getter (both undefined)") are false for that worker, and
the contract leaves it out. Also, the `top` sentence's "refuse every canvas" holds only for a getter
that returns null (`ownRealm` compares with null); one that returns anything else changes nothing.

**Fix:** say that the list gives examples, or list what `js/web3d.js` and `js/gl.js` call; move
`crypto.getRandomValues` to what is read at load; name `closed` beside `top` in the worker sentence,
with what each getter must return to refuse.

### F3. The rerun's file count is wrong

`critic14-rerun/README.md` 62 says "**0 differ in all 61 files**", and WEB3D.md 896-897 "no boolean
differs in any of the 61 result files (`critic14-rerun/booleans.txt`)". `booleans.txt` has 60
comparisons, one a line: 21 under `rerun/`, 31 `results/rerun-*.json`, and the fourteenth critic's
eight pages. The conclusion holds (0 differ in each of the 60). **Fix:** 60.

## Notes

- **The `top` check holds** against everything tried, on this round's page and every earlier one
  (rerun below, 0 booleans and 0 strings differ): a removed frame, plain or with `function closed()
  {}`, is refused in the removal's own task (`top` null; T1); a closed popup is refused from `close()`
  on, or, with `function closed() {}`, once `top` reads null, 3 ms later (T2); nested frames,
  `innerHTML = ''`, re-insertion, `moveBefore()`, a refused `close()`, prerendering (with no DevTools
  client: created and drew while prerendering, recovered after activation) and the back-forward
  cache, as the fourteenth critic tried them. Probe NV's new case gates: with `9a4ab9c7`'s
  `web3d.js` the rendered check passes 79 of 80 and fails NV alone, on that case (the removed copy
  records a claim after the removal, and this window's copy is refused both canvases, "already has a
  live renderer"; `control/at-9a4ab9c7/`).
- **A closed window's lost renderer returns false from `frame()`** rather than throwing "navigated
  away" (T2, after `top` reads null: "returned false"), since `frame()` checks the loss first.
  WEB3D.md says both ("returns false once the context is lost, and throws if ..."), and NV accepts
  either. Not a finding; the order could be stated.
- **Wording.** The round's README says the lost listener "clears `onLost` after calling it"; the code
  clears it just before the call (`js/web3d.js` 838-841). The effect is the same.
- **The CI-step gate.** The round's 53 controls and the fourteenth critic's seven reproduce here
  (`yaml-gate/results.txt`, `yaml-gate/critic14-results.txt`: 53 of 53; all seven refused). What it
  does not check (another step's commands, a Cargo config or wrapper written at run time, a
  `GITHUB_ENV` name built at run time, the actions' insides, lint settings in code or manifests,
  repository settings) is stated in WEB3D.md 1054-1059, so it is not a finding. None of
  `.cargo/config*`, `clippy.toml` or `rust-toolchain*` is tracked (`git ls-files`).
- **The branch's history.** The game repository's round-15 history check (outside this repository)
  records home-directory paths in 34 superseded blob versions from the branch's first commits; no
  file at `226b7c92` holds one, and the branch is on no remote yet. Whether to rewrite before a push
  is the owner's decision; it is not a finding against the tracked files.
- **The confidentiality scan** lives in the game repository, since its pattern names what it looks
  for. Its round-15 README calls the scan finished (controls 153 of 153) and its branch run
  preliminary until the round's last commit, which `226b7c92` is. Run here on `226b7c92` with this
  folder untracked beside it: 0 on every line, 1,725 files, the commit messages too. Its result and
  its controls' run are recorded there, outside this repository.
- **S3 is outside the contract by construction** (it patches built-ins); it measures only the list
  F2(a) quotes, with wrappers that behave as the built-ins.

## Gates

| Gate | Result |
| --- | --- |
| `cargo fmt --check` | exit 0 |
| clippy, CI's macOS row and its wasm32 step | exit 0 each |
| clippy `-p loadngo-web3d --all-features` on wasm32, aarch64 Linux, Windows MSVC and the host | exit 0 each |
| `cargo test`, debug and release | 75 passed each |
| `build.sh` x3, the third from an empty target directory | byte-identical: wasm `4d6594b5…`, `gl.js` `6542cb39…`, `web3d.js` `bdbf9ec6…`, `web3d.d.ts` `2f7be9a7…`; no local path |
| `three-reference.mjs --check` | 20 lines verbatim, output matches three 0.180.0 |
| `ci-step.rb` on the real workflow | exit 0 |
| `check.mjs` | exit 0: 80 of 80 probes, no `INVALID_OPERATION`, 2 pending errors raised on purpose, 0 contexts lost to the limit; steady allocation 0.09 sampled bytes a frame, none in `pkg/`; control 87.9 |
| `check.mjs --blind` | exit 3, the control read 0.59 |

The non-rendered gates are in `gates/gates-critic15.txt`, from `gates/run-gates.sh` (the fourteenth
critic's, renamed). The rendered checks are in `gates/rendered/` and `gates/blind/`, from
`gates/run-rendered.sh`, which runs them by gates.sh's commands with the round's contention sampler.
`gates.sh` was not run, since it rewrites the round's committed evidence.

**Timing: near-quiet.** `gates/rendered/contention-during.txt` saw only this run's headless Chrome
and the desktop Chrome window that was open before it. Chrome 157:

| Canvas | GPU ms mean | p50 | p95 | CPU ms p50 (p95) |
| --- | --- | --- | --- | --- |
| 1024x576, MSAA 4x | 0.866 | 0.863 | 0.887 | 0.145 (0.22) |
| 1366x768, MSAA 4x | 1.051 | 1.05 | 1.064 | 0.18 (0.25) |
| 1024x576, no MSAA | 0.595 | 0.581 | 0.613 | 0.14 (0.235) |

- The medians are within 0.004 ms of WEB3D.md's quiet table (Chrome 156), and the p95s within
  0.005 ms.
- Every frame took 16.67 ms. Each run drew 64 draws and 320,000 triangles, and the clear colour
  read exact (33, 36, 43).

## Every earlier critic's probes, rerun

`rerun-critics.sh` is the round's script, copied so that its output lands here (`rerun/`,
`results/rerun-*.json`, `rerun-critics.log`), with `run10-copy.mjs` and `tools/run.mjs` beside it;
the round's own page, `r15.html`, is run from its folder into `results/rerun-round15-r15.json`.

- All 64 runs exit 0.
- `booleans.sh` compares every boolean leaf with the round's rerun and its `r15.json`: 0 differ in
  all 61 files, 1,353 booleans (`booleans.txt`).
- `tools/leaf-diff.mjs` compares every string and null leaf of each page's result the same way
  (`strings.txt`): the only differences are the server's port and DevTools frame ids
  (`critic11-dd`, `critic12-dd2`, `critic12-per`, `round12-dd`); `r15.json`'s strings all match.
- The fourteenth critic's pages read as the round says: `c1-closed` A1 creates and draws, `c3-reopen`
  R4 reads "first restored" once at the next restore in each variant, `c2-prerender` holds with and
  without DevTools, and `c4-top` (`function top() {}` does not run; `top` reads null once removed),
  `c7-restore-gap`, `c5-top-fix` and `c6-r4-fix` reproduce.
- The game repository's scan controls (its round-15 `scan-controls.sh`, unchanged): 153 of 153
  hold, as the round recorded, with no traceback and every control file read.

## Claims checked and true

- 58 imports and 28 exported functions (counted from the wasm); 75 tests; the sizes table
  (`pkg/web3d.js` 69,944 bytes, 21,667 gzipped; `gl.js` and the wasm unchanged).
- The round's three hashes and "no local path"; its gate run (`../gates.txt`) and its timing figures
  against the committed `rendered-check.json` (0.855, 0.847, 0.884; 1.056, 1.048, 1.116; 0.566,
  0.565, 0.579; CPU p50 0.05, 0.175, 0.045 and p95 0.245, 0.255, 0.055); the medians within 0.02 ms
  and p95s within 0.05 ms of the quiet table; the fourteenth critic's within 0.03 and 0.04.
- The allocation figures: 0.09 sampled bytes a frame, 56 bytes from the check's own call
  (`demo.js:426`), control 88.9; the blind run's control 0.54 (the README's 0.5).
- `frame()` has gained, since the quiet table's commit (`a77fe0f9`), one property read and two getter
  calls (`GLOBAL[SHARED_KEY]`, `top`, `closed`), and `_inPlace` is two property reads and three getter
  calls, as WEB3D.md 371-373 says.
- T1, T2, T3 and T5 (`r15.json`) reproduce, and T4's 26 cases each read as the contract's item 1
  states: the 10 that recover are, for each event, the control, the bubbling listener on the window,
  the two non-capturing listeners on the canvas ahead of web3d's that stop only propagation, and the
  one after web3d's; each stopped lost event is lost for good with no hook; each stopped restored
  event's hooks come at the next restore.
- `top` is an own, non-configurable accessor of a window (T1); `closed` an own, configurable one,
  replaced by a page's `function closed() {}` (T1; `c1-closed`).
- WEB3D.md 614-617: on a cooperative page the renderer live at the loss gets its hook however the
  page orders its listeners (F1's rows: r1's `onContextRestored` once in each).
- The CI-step gate's controls (Notes); the round's corrections to `critic13-rerun/README.md`.

## Files

- `site/`: `s1-restored-create.html` (F1; `?pkg=fix` and `?pkg=old` run it against `fix-pkg/` and
  `at-9a4ab9c7-pkg/`), `s2-worker-closed.html` with `worker-closed.js` (F2(b)),
  `s3-builtins-per-call.html` (F2(a)).
- `fix-pkg/`: F1's trial, `web3d.js` with the restored listener changed, beside today's `gl.js` and
  wasm; not shipped. `at-9a4ab9c7-pkg/`: `web3d.js` as the fourteenth critic judged it, beside the
  same `gl.js` and wasm.
- `results/`: `s1-restored-create.json`, `s1-restored-create-fix.json`,
  `s1-restored-create-at-9a4ab9c7.json`, `s2-worker-closed.json`, `s3-builtins-per-call.json`, and the
  rerun's `rerun-*.json`.
- `gates/`: `run-gates.sh`, `gates-critic15.txt`, `run-gates.log`; `run-rendered.sh`, `rendered/`,
  `blind/`.
- `control/`: `run.sh`, which runs the rendered check on a temporary copy with `9a4ab9c7`'s
  `web3d.js` (`at-9a4ab9c7/`) and with F1's trial (`fix-pkg/`).
- `rerun-critics.sh`, `rerun-critics.log`, `rerun/`, `run10-copy.mjs`, `booleans.sh`, `booleans.txt`,
  `strings.txt`.
- `tools/`: `run.mjs` and `no-devtools.mjs` (the round's drivers, writing here), `leaf-diff.mjs`.
- `yaml-gate/`: the round's controls of `ci-step.rb`, rerun (`results.txt`, `critic14-results.txt`).

Run a page: `PLAYWRIGHT_CORE=.../playwright-core/index.mjs node tools/run.mjs <port> s1-restored-create.html`.
