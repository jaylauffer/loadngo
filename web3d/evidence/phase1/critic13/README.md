# Critic 13: phase 1 at `757d0100`, judged (2026-11-04)

Branch `web3d-phase1` at `757d0100`, working tree clean before this folder. Chrome 156.0.8078.80
headless (`channel: 'chrome'`, ANGLE over Metal, Apple M4 Pro); rustc 1.99.0, wasm-opt 133.

**Verdict: FAIL.** One wrong refusal inside the supported model (F1), one loss-recovery gap that
WEB3D.md does not list (F2), and figures cited to a file that no longer holds them (F3). Every gate
passes, every earlier probe reruns as the round says, and the round's F1 to F4 fixes hold against
everything else tried here.

## Findings

### F1. A frame removed during a create leaves its canvas claimed for good

`CLAIM._holds()` is `ownRealm()` (`js/web3d.js` 470), and `ownRealm()` (333) stays true in a
removed frame: the removed realm still reads its own state through its global
(`results/f2.json`, R0). A removed frame's awaits on a compiled module never finish, so its catch
never puts the owner back. The canvas, adopted into this window's document, is then refused by
this window's copy with "this canvas already has a live renderer; dispose it first", for good, and
there is no renderer to dispose.

- `results/f2c.json`, three trials each, at 0 ms and 300 ms after the removal: refused every time
  when the frame's copy had compiled the module already (any frame that made one renderer before),
  or when its module never arrives; created only when the frame was still fetching the module (the
  aborted fetch rejects, and the catch runs).
- Control (`results/f2b.json`): the same frame removed with no create begun, its canvas adopted
  here: created and drawn. A raw `getContext('webgl2')` on the refused canvas gives a live context,
  so only web3d's stale claim refuses it.
- WEB3D.md 228-237 and the comment at `js/web3d.js` 466-469 cover a navigated-away realm only.

**Fix, tried here:** in a window, `ownRealm()` also requires `!closed`, read through the getter
taken at load. `closed` reads true in a removed frame and false in an attached or navigated one
(`results/f2b.json`, `signals`). `fix-pkg/web3d.js` is today's `pkg/web3d.js` with only that change
(and `inThisDocument` calling `ownRealm()`): the refused cases are created and drawn, 6 of 6
(`results/f2-fix.json`), and all 80 probes pass against it (`results/probes-with-trial-fix.json`).
Add the case to probe NV.

### F2. A restored listener of the page's that reopens the document drops the restore's hooks

The documented gap (WEB3D.md 457-459, Limits 936-938) is a page's lost listener, running before
web3d's, that reopens the document. The restored event has the same gap, and WEB3D.md says "Two
cases are left" (453). A `webglcontextrestored` listener of the page's on the canvas, added before
web3d's, that calls `document.open()` erases web3d's restored listener mid-dispatch, so the
controller (`js/web3d.js` 802-810) never runs for that restore (`results/f1b.json`, E):

- the lost event was cancelled and the context is restored, but `onContextRestored` is not called
  (0 after 500 ms). A page that follows WEB3D.md (create again when `onContextRestored` comes) waits
  for good;
- a page that creates anyway gets the first renderer's hook at the next loss's restore, after the
  second renderer's `onContextLost` (the log: first lost, page listener reopens, second lost,
  first restored, second restored);
- control E0, no reopening: each hook once, on time.

A capture listener of the page's on the window that reopens is covered (f1.json D2 and E2): the
observer runs at the microtask checkpoint after that callback, before the event reaches the canvas.
So the gap is exactly a page listener on the canvas itself, added before web3d's, for either event.

**Fix:** state the restored case beside the lost one in both places. Or close both: handle the two
events also in a capturing listener on the canvas's root node (its document, or its shadow root),
which runs before any listener on the canvas, keeping the canvas's own pair for detached canvases
and handling each event once.

### F3. The allocation figures are the last round's, cited to this round's file

WEB3D.md 526-542 cite `web3d/evidence/phase1/rendered-check.json` and `blind-control/` for figures
that are `a77fe0f9`'s. The committed files read otherwise:

| WEB3D.md | Says | The committed file says |
| --- | --- | --- |
| 526 | steady 0.09 sampled bytes a frame | 0 |
| 529 | "the 56 sampled bytes are all the check's own measuring call" | no bytes from `demo/` at all |
| 537 | positive control read 89.7 | 86.1 |
| 538 | `--blind` read 0.6 | 0.4 |
| 540-542 | cold: wrappers 255.8, `web3d.js` 29.8, `gl.js` 2.2 ("same file") | 258.5, 30.5, 2.3 |

Each is in `git show a77fe0f9:web3d/evidence/phase1/rendered-check.json`. The conclusion holds
(the gate passes, `pkg/` allocates nothing); the numbers and the 56-byte sentence are false as cited.
**Fix:** quote the committed run, or cite `a77fe0f9`'s.

## Notes

- **A live renderer's canvas moved away and back, the other document reopened meanwhile**
  (`results/f1b.json`, I, with `createHTMLDocument()` and with another frame): `frame()` draws again
  once it is back, as WEB3D.md 272-273 says, but it has no listener, and the next loss is permanent.
  WEB3D.md's first gap (455-456) says the listeners come back at the next create, which a live
  renderer does not make. Moving a driven canvas is unsupported; state what it costs here.
- **"After each size it sets"** (WEB3D.md 196, 259; the `js/web3d.js` header and `take` comments):
  the check is after the width and height pair. `results/f4.json` H2: moved away on the width set
  and back on the height set, web3d set the height while the canvas was in the frame's document, and
  the create succeeded. Harmless (no event can come in that task); the wording is loose. The header's
  "after each await" is likewise after the last one.
- **What a failed create leaves:** besides the context, `EXT_disjoint_timer_query_webgl2` and
  `KHR_parallel_shader_compile` are enabled on a page's context by `bind` (`results/f4.json`, X,
  before false, after true). WEB3D.md 202-205 does not mention it.
- **The YAML gate** passes the real workflow and fails a step without `--all-targets`. It also
  passes a step that runs `exit 0` before the clippy line, or that has `continue-on-error: true`
  (checked on a copy of the workflow outside the repository). WEB3D.md 887 is true of a replaced
  command, not an added one. Nothing in the workflow does either today.
- **Attacks that held** (`results/f1.json`, `f1b.json`, `f2.json`, `f3.json`, `f4.json`):
  - reopenings three times in one task; from a microtask; from the page's own observer callback;
    from a message task between `loseContext()` and its event; from inside `onContextLost`; while a
    create awaits; from a second copy of web3d in the same realm; in open and closed shadow trees;
    by `document.write()` from this window while the frame was parsing (it reopens);
  - `document.write()` from a parser-inserted script while the frame is parsing (no reopening; the
    listeners stay; `site/parse.html`);
  - a capture listener on the window that reopens during the lost or the restored event;
  - accessors under the key on `HTMLCanvasElement.prototype`, before web3d loads and after (never
    run); `freeze` and `seal` (refused untouched); freezing after a create; a Proxy (refused);
    `structuredClone` and transfer (refused by the browser);
  - a move on the height set (refused, size put back, no listener); a loss on the width set (fails
    "lost", its listeners stay, its hook comes, the next create draws); a create and a dispose from
    that callback (refused, and allowed);
  - the initial `about:blank`: Chrome 156 did not keep the window when the frame loaded a page of
    this origin (`results/f2.json`, R3), so the old copy is refused there.
- **Out of scope, seen:** a copy in an isolated world (an extension's, here made over DevTools)
  creates a second live renderer on a canvas the page's copy drives, and the page's renderer then
  draws wrong (`results/f3.json`, I). WEB3D.md 311-314 states this. A page that defines the key on
  `EventTarget.prototype` before web3d loads stops the module loading (`globalThis[KEY] ??= {}`
  takes the inherited value); one that overwrites its window's key is refused as "navigated away"
  (`results/f3.json`, G).
- **A removed frame's live renderer** is finished: its context is lost with the frame and cannot be
  restored (`results/f2.json`, R2). That is the browser's doing, not web3d's.

## Gates (`gates/`)

`gates.sh` rewrites the round's committed evidence, so each gate was run on its own with its command.

| Gate | Result |
| --- | --- |
| `cargo fmt --check` | exit 0 |
| clippy: CI's macOS row, its wasm32 step, and `--all-features` on wasm32, aarch64 Linux, Windows MSVC, the host | exit 0 each (`gates/cargo.txt`) |
| `cargo test --locked -p loadngo-web3d`, debug and release | 75 passed each |
| `build.sh` x3, the third from a new empty target directory | byte-identical: wasm `4d6594b5…` (70,249 B, 32,174 gzipped), `gl.js` `6542cb39…`, `web3d.js` `65bb9a0c…`, `web3d.d.ts` `c46f3cc2…`; no local path (`gates/builds.txt`) |
| `three-reference.mjs --check` | 20 lines verbatim, output matches three 0.180.0 |
| the YAML gate's command (from `gates.sh`) | exit 0 on the real workflow (see the note) |
| `check.mjs` | exit 0: 80 of 80 probes, no `INVALID_OPERATION`, 2 pending errors on purpose, 0 contexts lost to the limit; steady 0.09 sampled bytes a frame over 601 frames, none in `pkg/`; control 88.35 (`gates/rendered/`) |
| `check.mjs --blind` | exit 3, control 0.6 (`gates/blind/`) |

`gates.sh` judges each gate by its exit status, and the blind run by exactly 3.

**Timing.** The three timed runs had one other Chrome alive, a desktop window; another session's
headless Chrome started later, during the probes and the allocation windows
(`gates/rendered/contention-during.txt`, every 5 s). So: near-quiet, not certified quiet. The blind
run's timings were contended (p95 up to 8.5 ms) and are not used.

| Canvas | GPU ms mean | p50 | p95 | CPU ms p50 (p95) |
| --- | --- | --- | --- | --- |
| 1024x576, MSAA 4x | 0.867 | 0.862 | 0.922 | 0.105 (0.245) |
| 1366x768, MSAA 4x | 1.023 | 1.021 | 1.029 | 0.045 (0.065) |
| 1024x576, no MSAA | 0.58 | 0.57 | 0.603 | 0.045 (0.205) |

GPU p50s are within 0.03 ms of WEB3D.md's quiet table and p95s within 0.04 ms; every frame took
16.67 ms; 64 draws and 320,000 triangles; the clear colour exact.

## Every earlier critic's probes, rerun

`rerun-critics.sh` is the round's own script, copied so that everything it writes lands here
(`rerun/`, `results/rerun-*.json`, `rerun-critics.log`), with `run10-copy.mjs` and
`rerun-tools/run.mjs` copied at the same depth. All 46 runs exit 0. Against the round's rerun
(`../critic12-rerun/`), 43 result files, 0 differing booleans (`booleans.txt`).

Each by-premise argument holds as the round states it. Spot-checked here: the fifth's X8
(`objectPlusTinyIsObject` true: `o + 2^-30` is the handle itself; `o + 0.5` is refused); the tenth's
colour-space cases (each `after: "srgb"`, so web3d reset it; the green-pixel test does not fit
them); the twelfth's pages read fixed (`docopen` every failure false, `dd2` refused both ways,
`midcreate` every case refused, `bf` restored from the cache and recovers). N10's intent page
passes every arrangement (`results/rerun-round12-intent.json`).

## Claims checked and true

The 58 imports and 28 exported functions (counted from the wasm); 75 tests; the sizes; the quiet
table against `../critic12/gates/rendered/rendered-check.json`, and its 0.02 and 0.04 ms comparisons
with `33df5d4a`'s; this round's contended figures against the committed `rendered-check.json`;
three's `WebGLRenderer.js` lines 276, 3399-3405 and 2501-2504; the unsafe code's files; the demo's
58 knots of 5,376 triangles; the DO, NV, MC, RC, TO and SY probes' data against the round's README.

## Phase 1 against its own bar

WEB3D.md's phase 1 is the skeleton: crate, WebGL2 binding, renderer API, lean shaders, GPU and CPU
timing, demo, build, gates. All of it is built and every gate holds. It fails here only on F1 to F3,
each narrow: one-line code fixes for F1 (tried above) and doc fixes for F2 and F3, with F2's code fix
optional.

## Files

- `site/`: the attack pages (`f1`, `f1b`, `f2`, `f2b`, `f2c`, `f2-fix`, `f3`, `f4`), with `frame.html`,
  `frame-fix.html`, `parse.html` and `common.js` (a copy of the twelfth critic's);
- `tools/`: `run.mjs` (the twelfth critic's driver, with an isolated-world hook), `serve.mjs` (the
  eleventh critic's server, with a slow-script path), `sampled.mjs` (the contention sampler);
- `results/`: each page's output, and the rerun's;
- `fix-pkg/`: the trial fix for F1, not shipped;
- `gates/`, `rerun/`, `booleans.txt`, `rerun-critics.log`.

Run a page: `PLAYWRIGHT_CORE=.../playwright-core/index.mjs node tools/run.mjs <port> f1.html`.
