# Critic 16: phase 1 at `dfeb1b91`, judged (2026-11-04)

Branch `web3d-phase1` at `dfeb1b91`, working tree clean before this folder. Chrome 157.0.8103.38
headless (`channel: 'chrome'`, ANGLE over Metal, Apple M4 Pro); rustc 1.99.0, wasm-opt 133. Judged
under the scope contract this critic was given, which takes WEB3D.md's page contract as the
supported model: a finding is a misbehaviour on a cooperative page, a false claim, a gate that does
not gate, evidence that does not support what cites it, or a name or home path in a tracked file;
new hostile-page cases are notes.

**Verdict: FAIL.** Every gate passes (near-quiet timings within 0.006 ms of the quiet table), every
earlier critic's page reruns as the round's own rerun did (67 runs; 0 of 1,396 booleans and no
string but ports, frame ids and a random serial differ), and the round's fix holds
for the fifteenth critic's page and for every case of its own. It fails on:

- F1, the same cooperative page as the fifteenth critic's F1 (its restored listener, ahead of
  web3d's, makes the next renderer), when it also loses the context again before that restore's
  dispatch reaches web3d's listener: the new renderer's `onContextRestored` is spent on the restore
  before it, while the context reads lost, and its own loss and restore call nothing. So do a
  renderer disposed, and a create that failed, in that window. Seven statements say otherwise, five
  of them new this round;
- F2, the page contract's item 2, as restated this round, says that every built-in web3d uses other
  than those taken at load and `crypto.getRandomValues` is read at each call, "among them ... the
  statics of `Object`": seven that `js/web3d.js` uses, three of them `Object`'s statics, are reached
  by no call;
- F3, this round's new sentence on `frame()`, "returns false once the context is lost (before any
  other check)", in four places: a disposed renderer's `frame()` throws "renderer disposed", lost or
  not, as WEB3D.md says elsewhere.

## Findings

### F1. A loss the page makes during the restore's dispatch, ahead of web3d's listener, spends the next renderer's hooks

**The page.** It is the fifteenth critic's S1 page, cooperative by the contract's three items (it
stops no event, reopens no document, patches no built-in, moves no canvas): a `webglcontextrestored`
listener of its own, added on the canvas before web3d's first create, makes the next renderer, r2.
The round's fix keeps r2 live. Then, still inside that restore's dispatch, the page loses the
context again with `WEBGL_lose_context.loseContext()`, as web3d's own probes and the critics' pages
do: in the create's `then`, which runs at the microtask checkpoint after the listener, or in a
second listener of its own ahead of web3d's. Two more cases do the same around a dispose and a
failed create. `site/g1-loss-in-dispatch.html`, `results/g1-loss-in-dispatch.json`.

**What happens.** When web3d's restored listener runs, the context reads lost again, but its lost
event is still to come. The listener finishes the current renderer if `_shim.lost()`, which is true
from `isContextLost()` alone, so it takes r2's `onContextRestored`, calls it, and forgets r2. At
r2's own lost event no renderer is current, so `onContextLost` is not called; at its restore
nothing is waiting. `dispose()` and a create that fails "lost" put their hook in the waiting set,
and the restored listener calls the whole set at the next restore it handles, which is the one
whose dispatch is running.

| Where the page loses r2's context, in the first restore's dispatch | r2's hooks today | 226b7c92 |
| --- | --- | --- |
| in the create's `then`, r1 disposed first | `onContextRestored` at the first restore, context reads lost; nothing at its own loss or restore | the same |
| the same, r1 not disposed | the same | the same |
| in a second listener of the page's ahead of web3d's, r1 disposed first | the same | the same |
| in the create's `then`, and r2 disposed there (so disposed while lost) | `onContextRestored` at the first restore, context reads lost; nothing at its own restore | the same |
| in the page's listener before its create, which fails "lost" | the failed create's `onContextRestored` at the first restore, context reads lost; nothing at the restore of the loss it failed on | the same |
| control: in a task after the dispatch | `onContextLost` at its loss; `onContextRestored` at its restore, context live | `onContextRestored` at the first restore (the fifteenth critic's F1); nothing at its own loss |

In every row r1's `onContextRestored` comes once, at the first restore, but in the first five the
context already reads lost again when it comes. The page here recovers only because it creates
again afterwards on its own; a page that waits for `onContextRestored` after r2's loss waits for
good, and a create made when the spurious call comes finds the context lost, as the hook's own
`isContextLost()` reads, and is refused "lost". The fifth row's refusal says "create the renderer
again when this call's onContextRestored is called, on the restore"; that call comes at once, while
the context is lost.

This is not a regression: `results/g1-loss-in-dispatch-old.json` runs the page against `226b7c92`'s
`web3d.js` (`at-226b7c92-pkg/`), where the same five rows fail the same way and the control fails
too. It is the part of the fifteenth critic's F1 that the round's fix does not cover. The trigger is
narrow: the page itself loses the context, synchronously, inside that dispatch.

**Claims this makes false:**

- WEB3D.md 619-623, new this round: a renderer made by a restored listener of the page's ahead of
  web3d's "stays live, and its hooks are for its own later losses"; and 1004-1006, also new: "which
  keeps its hooks for its own loss".
- `js/web3d.d.ts` 57-58, new: "a renderer made there, even before web3d's listener runs, has its
  hooks for its own later losses"; 54-56, on `onContextRestored`: "Called once when the context is
  restored after a loss of this renderer's" (r2's comes at a restore before its loss, while lost;
  so does that of r2 disposed while lost, and of the failed create); and 49-50, on `onContextLost`:
  "Called when the canvas's context is lost while this renderer is the canvas's live one" (r2 is
  live when the page loses its context, and hears nothing).
- `js/web3d.js` 825-828 (`controllerFor`, new) and 854-855 (the restored listener, new): a renderer
  "made since the loss was counted is live on the restored context and stays current". r2 was made
  after the loss was counted, and is not kept.

**Fix, tried here** (each `fix-pkg*/web3d.js` beside today's `gl.js` and wasm; not shipped):

| Trial | Change | G1's six rows | Rendered check |
| --- | --- | --- | --- |
| `fix-pkg/` | `take()` records the loss count r2 took its context at on `controller.current`; the restored listener finishes `current` only if the canvas's count has moved past it (not on `isContextLost()`) | the first three and the control as documented; the dispose and failed-create rows still fail (`results/g1-loss-in-dispatch-fix.json`) | 80 of 80, steady allocation 0.09, control 89.2 (`control/fix-pkg/`) |
| `fix-pkg-2/` | that, and each waiting hook tagged with the loss whose restore it waits for (a renderer's own count plus one); the restored listener calls only those whose loss is counted | all but the failed-create row (`-fix2.json`) | 80 of 80, steady 0.16, control 89.6 (`control/fix-pkg-2/`) |
| `fix-pkg-3/` | `fix-pkg/`, and the restored listener returns at once, calling nothing and keeping `current`, when the context (kept on the controller at `take()`) reads lost by the time it runs: the page has lost it again, and every hook waits for the restore after that loss | all six as documented; r1's `onContextRestored` then comes once, at the next restore, with the context live (`-fix3.json`) | 80 of 80, steady 0, control 89.1 (`control/fix-pkg-3/`) |

`fix-pkg-3/` is the smallest that covers every row: five lines of code changed or added.

### F2. Item 2 says every other built-in web3d uses is read at each call; seven are read only at load

WEB3D.md 143-150, restated this round for the fifteenth critic's F2(a): "web3d takes the getters and
methods its checks rely on when it loads (the module-level constants in `js/web3d.js`) ...; it also
calls `crypto.getRandomValues` once then, to start its serial counter. Every other built-in it uses
it reads at each call, among them ... the statics of `Object`, `Reflect`, `Number`, `Array`,
`ArrayBuffer` and `Math` ...".

`site/b1-builtins-at-load.html` is the converse of the fifteenth critic's S3: after web3d has loaded
it replaces built-ins that `js/web3d.js` uses only while loading (to build its constants and shared
state, not as a getter or method a check relies on) with wrappers that count the calls made from
`js/web3d.js` or `js/gl.js` (by the stack) and otherwise behave as the built-in, then makes ordinary
calls: a first create on a canvas, draws, a refused call, a call the Rust side refuses, 30 frames, a
loss and restore, a dispose, a new create, `compile()` given bytes and given another URL.
`results/b1-builtins-at-load.json`:

| Built-in | Used by `js/web3d.js` | Calls from web3d |
| --- | --- | --- |
| `Set` (positive control) | `controllerFor`, the restored listener | 2 |
| `Map` | `ALPHA_MODES`, `WRAPS`, `COMPILED` | 0 |
| `WeakMap`, `WeakSet` | the realm's controllers and watched documents | 0 |
| `Symbol.for` | `SHARED_KEY` | 0 |
| `Object.freeze` | `CLAIM`, `WATCH` | 0 |
| `Object.getPrototypeOf` | `TYPED_ARRAY` | 0 |
| `Object.getOwnPropertyDescriptor` | `getter`, `setter` (every getter taken at load) | 0 |
| `crypto.getRandomValues` | the serial counter's start | 0, as the sentence says |

The four calls to `Map` that did reach the wrapper came from the page driver's own evaluation script,
not from web3d (recorded with their stacks). The round's README says the same of `Map` ("web3d makes
its maps at load"), so the sentence contradicts the round's own note. The direction is harmless (a
page is told that more is read at each call than is), but the sentence is false, and three of the
seven are statics of `Object`, which it names. **Fix:** say that the rest are read either while web3d
loads (to build its constants and shared state) or at each call.

### F3. "Returns false once the context is lost (before any other check)": a disposed renderer throws

New this round, for the fifteenth critic's note on a closed window: WEB3D.md 100 ("It returns false
once the context is lost (before any other check)"), `js/web3d.d.ts` 189 ("false from the context's
loss on, for good (before any other check)"), `js/web3d.js` 1411 ("Once lost it returns false before
any other check"), and `critic15-rerun/README.md` 20. `frame()` starts with `_enter()`, which checks
first that the renderer is not disposed and not inside another of its calls. `site/o1-frame-order.html`
(`results/o1-frame-order.json`) disposes the lost renderer in `onContextLost`, as "Context loss"
says, and calls `frame()` once more, as an animation loop does before it sees the hook's flag:

| The lost renderer | `frame()` while lost | after the restore |
| --- | --- | --- |
| left undisposed (control) | returned false | returned false |
| disposed in `onContextLost` | threw "web3d: renderer disposed" | the same |

WEB3D.md's "After `dispose()` every method throws" is right; the new parenthesis contradicts it.
**Fix:** "before its document and window checks".

## Notes

- **A lost event the page stops and cancels itself** (outside item 1;
  `site/n1-stopped-and-cancelled.html`, `results/n1-stopped-and-cancelled.json`, and `-old.json` at
  `226b7c92`). A capturing listener on the window stops the lost event and calls `preventDefault()`
  itself, so the context is restored.
  If nothing asked the renderer while the context was lost, its shim never latched the loss: after
  the restore `lost` reads false, `frame()` returns true and draws nothing with the lost context's
  objects (INVALID_OPERATION warnings, `getError` 1282), and a create is refused "already has a live
  renderer". With a `frame()` call while lost, the renderer reads lost after, and a create draws.
  Since this round's change the undisposed renderer's `onContextRestored` no longer comes in the
  first variant; at `226b7c92` it did. Item 1's "If it does" says such an event "is not cancelled,
  so the context is lost for good", which is true of web3d's part; it is silent on what follows
  when the page cancels it. A new hostile case, so not a finding.
- **The worker sentence** (item 2): "a `closed` one that returns true" refuses; any truthy value does
  (`!CLOSED.call(GLOBAL)` in `ownRealm`). Wording.
- **The gates' header** (`gates.sh` 13-14) says `critic13-rerun/rerun-critics.sh` runs the critics'
  scripts; it does, for the first thirteen, and WEB3D.md names the current one,
  `critic15-rerun/rerun-critics.sh`.
- **Paths in logs.** The rerun logs write the home directory as `~` by design, so they show the
  layout under it (`~/pudding/loadngo/...`) but no account; the round's and the fifteenth critic's
  do the same. Control logs name the per-user temporary directory the copy was served from
  (`/private/var/folders/...`), here as in `critic15-rerun/control/at-226b7c92/check.log` and the
  fifteenth critic's. It is not a home directory path.
- **Steady allocation.** This run read 0.09 sampled bytes a frame, all 56 bytes the check's own call
  (`demo.js:426`), as the fifteenth round's did; the round's committed run read 0. WEB3D.md reports
  each run as it read, and both are far under the gate's 1.
- **The branch's history.** As the fifteenth critic recorded: home-directory paths in 34 superseded
  blobs from the branch's first commits, none in a file at `dfeb1b91`, the branch on no remote. The
  owner's decision before a push.
- **The confidentiality scan** lives in the game repository, since its pattern names what it looks
  for. Run once on `dfeb1b91` with this folder untracked beside it: 0 on every line, 2,063 files
  (1,872 in the round's run on a clean tree), the commit messages too; its controls, rerun
  unchanged, 153 of 153. Both runs are recorded there, outside this repository.

## Gates

| Gate | Result |
| --- | --- |
| `cargo fmt --check` | exit 0 |
| clippy, CI's macOS row and its wasm32 step | exit 0 each |
| clippy `-p loadngo-web3d --all-features` on wasm32, aarch64 Linux, Windows MSVC and the host | exit 0 each |
| `cargo test`, debug and release | 75 passed each |
| `build.sh` x3, the third from an empty target directory | byte-identical: wasm `4d6594b5…`, `gl.js` `6542cb39…`, `web3d.js` `cced8579…`, `web3d.d.ts` `e1b9e4f3…`; no local path |
| `three-reference.mjs --check` | 20 lines verbatim, output matches three 0.180.0 |
| `ci-step.rb` on the real workflow | exit 0 |
| `check.mjs` | exit 0: 80 of 80 probes (LR's new case among them), no `INVALID_OPERATION`, 2 pending errors raised on purpose, 0 contexts lost to the limit; steady allocation 0.09 sampled bytes a frame, none in `pkg/`; control 90.5 |
| `check.mjs --blind` | exit 3, the control read 0.67 |

The non-rendered gates are in `gates/gates-critic16.txt`, from `gates/run-gates.sh` (the fifteenth
critic's, renamed). The rendered checks are in `gates/rendered/` and `gates/blind/`, from
`gates/run-rendered.sh`, which runs them by gates.sh's commands with the round's contention sampler.
`gates.sh` was not run, since it rewrites the round's committed evidence.

**Timing: near-quiet.** `gates/rendered/contention-during.txt` saw only this run's headless Chrome
and the desktop Chrome window that was open before it; no other check of mine ran during it.

| Canvas | GPU ms mean | p50 | p95 | CPU ms p50 (p95) |
| --- | --- | --- | --- | --- |
| 1024x576, MSAA 4x | 0.869 | 0.867 | 0.888 | 0.125 (0.18) |
| 1366x768, MSAA 4x | 1.053 | 1.051 | 1.069 | 0.15 (0.22) |
| 1024x576, no MSAA | 0.589 | 0.582 | 0.604 | 0.11 (0.205) |

- The means, medians and p95s are within 0.006 ms of WEB3D.md's quiet table (Chrome 156): the
  round's wider tail (0.96, 1.219, 0.917 ms at p95) did not recur here, which supports its
  inference that the desktop window's own work made it.
- Every frame took 16.67 ms. Each run drew 64 draws and 320,000 triangles, and the clear colour
  read exact (33, 36, 43).

## Every earlier critic's probes, rerun

`rerun-critics.sh` is the round's script, copied so that its output lands here (`rerun/`,
`results/rerun-*.json`, `rerun-critics.log`), with `run10-copy.mjs` and `tools/run.mjs` beside it:
every earlier critic's scripts and pages, the twelfth, fourteenth and fifteenth rounds' pages, and
the fifteenth critic's three.

- All 67 runs exit 0.
- `booleans.sh` compares every boolean leaf with the round's own rerun (`../critic15-rerun/`): 0
  differ in all 64 files, 1,396 booleans (`booleans.txt`).
- `strings.sh` compares every string and null leaf of each page's result the same way, with the
  fifteenth critic's `tools/leaf-diff.mjs` (`strings.txt`): 38 of 43 files read the same; the other
  five differ only in the server's port, DevTools frame ids (`critic11-dd`, `critic12-dd2`,
  `critic12-per`, `round12-dd`) and a handle's random serial in a refusal (`s3-builtins-per-call`).
- The fifteenth critic's pages against its own results: `s1-restored-create`'s three failing rows
  read fixed (r2's spurious `onContextRestored` gone, its own `onContextLost` and
  `onContextRestored` at its loss), `s2-worker-closed` reads the same, and `s3-builtins-per-call`
  differs only in that serial, as the round says.

## Claims checked and true

- The round's gate run (`../gates.txt`, on `7e2a25d4`; only `docs/WEB3D.md` changed outside the
  evidence since): every gate held; the three hashes; its rendered figures against the committed
  `rendered-check.json` (GPU 0.88, 0.868, 0.96; 1.087, 1.053, 1.219; 0.653, 0.589, 0.917; CPU p50
  0.21, 0.2, 0.185 and p95 0.285, 0.265, 0.345); the medians within 0.005 ms of the quiet table;
  the means and p95s above it and above the fifteenth round's run (whose p95s were 0.884, 1.116 and
  0.579, and medians within 0.02 ms and p95s within 0.05 ms of the table); allocation 0 at steady
  state, control 89.0; the blind run's 0.5; the cold window's 257.6, 29.9 and 2.9 sampled bytes a
  frame; the fifteenth round's 0.09 and 56 bytes; the sizes (`web3d.js` 70,925 bytes, 21,965
  gzipped).
- The round's control (`critic15-rerun/control/at-226b7c92/`): with `226b7c92`'s `web3d.js` the
  rendered check fails on LR alone, 79 of 80, where r2 gets a spurious `r2 restored` at the first
  restore and nothing at its own; today LR's two new cases read `r1 lost`, `r2 created`, `r1
  restored`, then `r2 lost`, `r2 restored`, and a new renderer draws. So LR gates the fifteenth
  critic's F1.
- The round's rerun: 67 runs, each exit 0; 1,396 booleans in 64 files, 0 differ (`booleans.txt`);
  `s1-restored-create`'s three failing rows now give r2 its own `onContextLost` and
  `onContextRestored`; `s2-worker-closed` reads with no string changed, and `s3-builtins-per-call`
  only a handle's serial (`tools/leaf-diff.mjs`).
- The fifteenth critic's F3: 60 files, in WEB3D.md and `critic14-rerun/README.md`; `onLost` cleared
  just before its call (`js/web3d.js` 846-848).
- `take()`'s restated reason: a renderer still current at a create is one finished by a loss (a live
  one holds the canvas and refuses the create; a disposed one is no longer current); in the
  contract, its restore is the one being dispatched, and web3d's listener calls the hook later in
  it (G1's control row, and S1's rows).
- "`Map` is not listed as a constructor: web3d makes its maps at load", and `crypto.getRandomValues`
  called only at load (B1: no call reaches either).
- The worker sentence's cases (the fifteenth critic's `s2-worker-closed`, rerun).
- WEB3D.md's status (fifteen critic rounds), its fifteenth critic's Results row, and its by-hand
  list (`critic15-rerun/rerun-critics.sh`, `booleans.sh`).

## Files

- `site/`: `g1-loss-in-dispatch.html` (F1; `?pkg=fix`, `fix2`, `fix3` and `old` run it against
  `fix-pkg/`, `fix-pkg-2/`, `fix-pkg-3/` and `at-226b7c92-pkg/`), `b1-builtins-at-load.html` (F2),
  `o1-frame-order.html` (F3), `n1-stopped-and-cancelled.html` (the note; `?pkg=old`).
- `fix-pkg/`, `fix-pkg-2/`, `fix-pkg-3/`: F1's trials, `web3d.js` changed, beside today's `gl.js` and
  wasm; not shipped. `at-226b7c92-pkg/`: `web3d.js` as the fifteenth critic judged it, beside the
  same `gl.js` and wasm.
- `results/`: `g1-loss-in-dispatch.json` and its `-fix`, `-fix2`, `-fix3` and `-old` runs,
  `b1-builtins-at-load.json`, `o1-frame-order.json`, `n1-stopped-and-cancelled.json` and `-old`, and
  the rerun's `rerun-*.json`.
- `gates/`: `run-gates.sh`, `gates-critic16.txt`, `run-gates.log`; `run-rendered.sh`, `rendered/`,
  `blind/`.
- `control/`: `run.sh`, which runs the rendered check on a temporary copy with each trial's
  `web3d.js` (`fix-pkg/`, `fix-pkg-2/`, `fix-pkg-3/`), and its logs (`run.log` for the first two,
  `run-fix-pkg-3.log`).
- `rerun-critics.sh`, `rerun-critics.log`, `rerun/`, `run10-copy.mjs`, `booleans.sh`, `booleans.txt`,
  `strings.sh`, `strings.txt`.
- `tools/`: `run.mjs` and `no-devtools.mjs` (the round's drivers, writing here), `leaf-diff.mjs`.

Run a page: `PLAYWRIGHT_CORE=.../playwright-core/index.mjs node tools/run.mjs <port> g1-loss-in-dispatch.html`.
