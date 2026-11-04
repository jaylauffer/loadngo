# Critic 14: phase 1 at `9a4ab9c7`, judged (2026-11-04)

Branch `web3d-phase1` at `9a4ab9c7`, working tree clean before this folder. Chrome 157.0.8103.38
headless (`channel: 'chrome'`, ANGLE over Metal, Apple M4 Pro); rustc 1.99.0, wasm-opt 133.

**Verdict: FAIL**, on narrow items. Every gate passes, every earlier critic's probe reruns as the
round says (0 booleans differ from the round's own rerun), and the `closed` fix holds against
every removal, move, popup, prerender and refused close tried here. It fails on:

- F1, a page's global `function closed() {}` turns the removed-frame check off;
- F2, the reopening limit is not stated exactly, and the reason for leaving it open does not hold;
- F3, a page listener that stops the event's propagation defeats the loss controller, and this is
  not documented;
- F4, the round's README names the wrong browser.

## Findings

### F1. A global `function closed() {}` turns off the removed-frame check

`CLOSED = getter(globalThis, 'closed')` (`js/web3d.js` 319) takes the window's own `closed`
accessor. That accessor is configurable. A classic script's top-level `function closed() {}`
replaces it with a non-configurable data property. This is ordinary page code: no prototype is
rewritten and no `defineProperty` is called. Then `CLOSED` is `undefined`, and `ownRealm()` (340)
treats the window as a worker, so it skips the check without saying so. The thirteenth critic's F1
comes back.

`results/c1-closed.json`, A1, a frame whose page declares the function before web3d loads:

- the descriptor reads "data: function, not configurable";
- once the frame is removed, its copy's create gets past the check and never settles;
- this window's copy is refused the canvas: "already has a live renderer".

Declared after load (A1b), or as `var closed` (A1c), it is harmless. A `defineProperty` before
load (A1d) does the same as A1, but that is patching a built-in, which is out of scope.

These claims are false in that case:

- WEB3D.md 244-246: "through the built-in `closed` getter taken when the module loads";
- WEB3D.md 246-248: "Once ... its frame has been removed: `createRenderer` refuses every canvas";
- the comment at `js/web3d.js` 315-317.

**Fix, tried here:** read `top` instead:
`GLOBAL[SHARED_KEY] === REALM_STATE && GLOBAL.top !== null`.

- `top` is unforgeable: own and non-configurable. A page's `function top() {}` throws
  ("Identifier 'top' has already been declared"; `results/c4-top.json`).
- It reads null once the frame is removed, even with `function closed` declared.
- It is `undefined` in a worker.

`fix-pkg/web3d.js` changes only that line. With it, `results/c5-top-fix.json` creates in A1, and
every other case reads as before. It was not run against the 80 probes.

### F2. The reopening limit is not stated exactly, and its reason does not hold

**(a) The restored case's consequence is stated wrongly.** WEB3D.md 501-506 says "the controller
stays as it was before the restore, so the hooks it held come with the canvas's next restore".
`results/c3-reopen.json`, R4, checks what the page does after the missed restore:

| The page, after the missed restore | What happens |
| --- | --- |
| disposed in `onContextLost`, then creates | the first renderer's hook comes at the next restore, as stated |
| disposes, then creates | the same, as stated |
| creates without disposing | the first renderer's `onContextRestored` never comes. `take()` (`js/web3d.js` 736) overwrites `controller.current` |
| neither disposes nor creates | the first renderer's `onContextLost` is called a second time at the next loss. WEB3D.md 524-526 says a lost renderer left undisposed "hears no later loss or restore" |

**Fix, tried here:** in `take()`, before `controller.current` is set, add
`if (controller.current) waitForRestore(controller, controller.current.onRestored);`.

`fix-pkg-r4/` has this change, and `results/c6-r4-fix.json` reads "first restored" in every
variant. It is safe because `current` can be non-null there only when a restore was never handled:

- in Chrome the context never reads restored before its restored event has run;
- `results/c7-restore-gap.json` polled `isContextLost()` in tasks of their own, about 180,000
  times in each of 20 restores, and saw a live context before the event 0 times;
- the WebGL specification clears the lost flag and fires the event in one task.

Also state the double `onContextLost`, or skip the call for a renderer that was already finished.

**(b) The reason not to fix does not hold.** WEB3D.md 510-515 says closing the two cases "would take
a listener that runs before every listener of the page's on the canvas" on the canvas's root node.
The round's README rejects an observer fix because "it cannot tell a missed restore from one whose
event is still to come". Both are wrong:

- **The lost event, outside shadow trees.** web3d's observer runs at the microtask checkpoint after
  the page's reopening listener, while the event is still being dispatched. At that point
  `window.event` is that very event. `results/c3-reopen.json` R6 tried this with an observer of the
  canvas's realm. It saw the trusted `webglcontextlost` event at the canvas (phase 2, not yet
  prevented), and its `preventDefault()` brought the context back. The common case is a canvas in
  the document, with page code in the canvas's realm, and this is enough there. Handle each event
  once (a `WeakSet` of events) so that the re-added listener does not handle it again.
- **The restored event, everywhere.** No task sees a restored context before its event has run
  (c7). So a check in a later task, scheduled by the observer, that finds the context live and the
  restore unhandled, knows the restore was missed and not still to come.
- **What only a root-node listener closes:** a lost event for a canvas in a shadow root. There
  `window.event` is undefined, as the DOM standard says (R6, shadow root).

Either fix the cases this way, or restate the reason truthfully.

**Wording, a nit.** "Added before the first create, or after a reopening and before the observer has
run" (WEB3D.md 496) leaves out a third way: added after a create that failed and took web3d's
listeners off, before the next create.

**What holds as stated** (c3-reopen R1-R3):

- nested shadow roots: a reopener on the inner root is covered, and one on the outer root never sees
  these events, which are not composed;
- a listener added in the reopening's task sits ahead of web3d's re-added one. It is harmless unless
  it reopens again, and then it is the stated limit;
- reopening another document leaves this canvas covered;
- reopening the parent document, which holds this frame, removes the frame.

### F3. A page listener that stops propagation defeats the loss controller, and this is not documented

`results/c3-reopen.json`, R5:

- a capturing listener on the window that calls `stopPropagation()` during the lost event: the
  context is never restored, and no hook runs;
- a listener on the canvas ahead of web3d's that calls `stopImmediatePropagation()`: the same;
- during the restored event: the context comes back, but `onContextRestored` never comes;
- control, a listener ahead that only calls `preventDefault()`: it recovers.

These claims are false in that case:

- `js/web3d.js` 48-49: the controller "cancels every lost event and calls the hooks";
- WEB3D.md 528: "the renderer live at the loss gets its hook however the page orders its own
  listeners".

The Limits list only reopenings. No code can override a page listener that runs first and stops
the event. **Fix:** state it as a limit. F2(a)'s fix also keeps the restored variant's hook for the
next restore.

### F4. The round's README names the wrong browser

`critic13-rerun/README.md` 3-4 says "Chrome 156.0.8078.80". Every result of the round records
157.0.8103.38:

- the 31 page results;
- `rendered-check.json` and `blind-control/`;
- the control.

WEB3D.md 772-773 argues that the GPU's work has not changed since the table's run (Chrome 156)
without saying that the browser changed. The conclusion still holds: this critic's near-quiet run
on 157 matches the table within 0.03 ms (Gates, below). **Fix:** correct the README, and say in
WEB3D.md that the browser changed.

## Notes

- **The CI-step gate passes workflows whose step cannot fail, or never runs** (`yaml-gate/`). Seven
  changed copies pass `ci-step.rb`:
  - `RUSTFLAGS: --cap-lints warn` in the `env` of the step, the job or the workflow;
  - an earlier step that writes that line to `$GITHUB_ENV`;
  - a job `if: false`;
  - `branches` filters that match nothing;
  - `paths-ignore: ['**']`.

  `yaml-gate/cap-lints.txt` shows that the flag makes `cargo clippy ... -- -D warnings` exit 0 on a
  lint. The real workflow does none of these. WEB3D.md 945-953 ("Nothing may let the step pass
  when that command fails ... in a workflow that runs on pushes and pull requests") overstates what
  the gate checks. The round's eight controls reproduce (`yaml-gate-round14/`). Its `run.sh` counts
  any failure of the gate as a pass, even a Ruby `LoadError`; only the real-workflow control would
  catch a broken gate.
- **The confidentiality scan** passes on the branch: 0 on every line, 1,238 files, commit messages
  both ways. Its gaps are recorded beside the scan, outside this repository, since its pattern
  names what it looks for. A wider scan of the branch found nothing.
- **`closed` attacks that held** (`results/c1-closed.json`, `results/prerender-no-devtools.json`):
  - removed in the same task as its first create (the fetch aborts, and the claim is released);
  - a nested frame whose parent frame is removed, or whose own iframe element is removed;
  - `document.body.innerHTML = ''`;
  - re-inserted by `append`, which makes a new browsing context;
  - after each removal, this window's copy creates and draws;
  - kept attached by `moveBefore()`: still draws and recovers;
  - a frame's own `window.close()`, which is ignored: still draws;
  - a popup draws while open, and its `closed` reads true at once after `close()`;
  - a top window whose `close()` is refused: `closed` stays false, and it draws;
  - a prerendered page (`document.prerendering` true): `closed` false, creates and draws during
    prerendering, then draws and recovers after activation.

  Playwright's DevTools attachment disables prerendering ("PrerenderingDisabledByDevTools",
  `tools/prerender-status.mjs`), so `tools/no-devtools.mjs` runs Chrome with no DevTools client.
  The back-forward cache is covered by the twelfth critic's `bf.html`, rerun here: restored from
  the cache, and each copy, a frame's too, draws and recovers.
- **Polish.**
  - A closing popup's `frame()` throws the "navigated away, or a removed frame" message.
  - One comment line in `js/web3d.d.ts` (the `createRenderer` comment) is not wrapped.
  - dolores' figures predate two shader changes and the present load (WEB3D.md says so).

## Gates

| Gate | Result |
| --- | --- |
| `cargo fmt --check` | exit 0 |
| clippy, CI's macOS row and its wasm32 step | exit 0 each |
| clippy `-p loadngo-web3d --all-features` on wasm32, aarch64 Linux, Windows MSVC and the host | exit 0 each |
| `cargo test`, debug and release | 75 passed each |
| `build.sh` x3, the third from an empty target directory | byte-identical: wasm `4d6594b5…`, `gl.js` `6542cb39…`, `web3d.js` `91f5bc13…`, `web3d.d.ts` `d44794fb…`; no local path |
| `three-reference.mjs --check` | 20 lines verbatim, output matches three 0.180.0 |
| `ci-step.rb` on the real workflow | exit 0 |
| `check.mjs` | exit 0: 80 of 80 probes, no `INVALID_OPERATION`, 2 pending errors raised on purpose, 0 contexts lost to the limit; steady allocation 0.16 sampled bytes a frame, none in `pkg/`; control 89.1 |
| `check.mjs --blind` | exit 3, the control read 0.7 |

The non-rendered gates are in `gates/gates-critic14.txt`, from `gates/run-gates.sh`. The rendered
checks are in `gates/rendered/` and `gates/blind/`. `gates.sh` was not run, since it rewrites the
round's committed evidence.

**Timing: near-quiet.** During the three timed runs, `contention-during.txt` saw only this run's
headless Chrome and a desktop Chrome window. Other sessions' headless Chromes appeared later, during
the allocation windows. Chrome 157:

| Canvas | GPU ms mean | p50 | p95 | CPU ms p50 (p95) |
| --- | --- | --- | --- | --- |
| 1024x576, MSAA 4x | 0.847 | 0.845 | 0.866 | 0.045 (0.06) |
| 1366x768, MSAA 4x | 1.024 | 1.023 | 1.033 | 0.045 (0.06) |
| 1024x576, no MSAA | 0.569 | 0.568 | 0.582 | 0.045 (0.06) |

- These medians are within 0.03 ms of WEB3D.md's quiet table (Chrome 156), and the p95s within
  0.04 ms.
- The round's contended medians (0.475, 0.318, 0.569) were artifacts of contention.
- Every frame took 16.67 ms. Each run drew 64 draws and 320,000 triangles, and the clear colour
  read exact.

## Every earlier critic's probes, rerun

`rerun-critics.sh` is the round's script, copied so that its output lands here (`rerun/`,
`results/rerun-*.json`, `rerun-critics.log`), with `run10-copy.mjs` and `tools/run.mjs` beside it.

- All 55 runs exit 0.
- `booleans.sh` compares every boolean leaf with the round's own rerun (`../critic13-rerun/`):
  0 differ in all 53 files (`booleans.txt`).
- `f2c`: all 36 trials create.

**By-premise arguments.** Each one checked here holds:

- `f1`'s P2, G and D2 read `lostEvent` "none" with each hook once and the context restored;
- E3's `restored` is the page's own erased listener;
- the twelfth critic's `blank` `b2` is R2 here: a listener added in the reopening's task runs
  before web3d's re-added one;
- the rest agree with the thirteenth critic's checks.

## Claims checked and true

- 58 imports and 28 exported functions (counted from the wasm); 75 tests; the sizes table;
  `web3d.js` 1,448 bytes larger than `757d0100`'s (451 gzipped, with the file name in the header).
- three r180's `WebGLRenderer.js` lines 276, 2501-2504 and 3399-3405.
- The quiet table against `critic12/gates/rendered/`; its 0.02 and 0.04 ms against `33df5d4a`'s;
  the thirteenth critic's 0.03 and 0.04 ms; the round's contended GPU and CPU figures.
- The allocation figures, against the committed `rendered-check.json`: 0.09, 56 bytes, 88.9, 0.6,
  257.6, 29.9, 1.9.
- The NV control: 79 of 80, four canvases refused.

## Phase 1 against its own bar

WEB3D.md's phase 1 is the skeleton: crate, WebGL2 binding, renderer API, lean shaders, GPU and CPU
timing, demo, build, gates. All of it is built, and every gate holds. It fails only on F1 to F4.
Each is narrow, and only F1 touches the code that ships in a frame. F1 and F2(a) are one-line
fixes, tried here. F2(b) and F3 are documentation, or a short observer change.

## Files

- `site/`: the attack pages. `c1-closed`, `c2-prerender`, `c3-reopen`, `c4-top`, `c5-top-fix`,
  `c6-r4-fix` and `c7-restore-gap`, with their frames and `popup.html`.
- `tools/`: `run.mjs` (the thirteenth critic's driver, writing here); `no-devtools.mjs` (Chrome
  with no DevTools client, for prerendering); `prerender-status.mjs`.
- `fix-pkg/` (F1's trial) and `fix-pkg-r4/` (F2(a)'s trial), not shipped.
- `results/`, `rerun/`, `booleans.*`, `rerun-critics.*`, `gates/`, `yaml-gate/` and
  `yaml-gate-round14/`.

Run a page: `PLAYWRIGHT_CORE=.../playwright-core/index.mjs node tools/run.mjs <port> c3-reopen.html`.
