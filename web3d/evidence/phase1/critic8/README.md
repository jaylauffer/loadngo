# Eighth critic, web3d phase 1 (2026-11-03), generic evidence

Judged: loadngo `web3d-phase1` at `c4e76ce2`. Chrome 156.0.8078.80 headless (`channel: 'chrome'`,
ANGLE over Metal, Apple M4 Pro); rustc 1.99.0, wasm-opt 133. Other sessions used the Mac at the same
time, so GPU times here are not a capacity measure. Anything about the application this renderer is
built for is kept in that application's own repository. Verdict: **FAIL**.

## Gates (all rerun here)

| Gate | Result | File |
| --- | --- | --- |
| `cargo fmt --check` (workspace, and `-p loadngo-web3d`) | pass | `gates.txt` |
| clippy `-D warnings`: CI's macOS-row workspace command; CI's wasm32 step; `-p loadngo-web3d` on the host | pass | `gates.txt` |
| `cargo test --locked -p loadngo-web3d`, debug and release | 74 pass each | `gates.txt` |
| `build.sh` twice in place, once from an empty target dir | all four `pkg/` files byte-identical to the pkg found (sha256); no local path in the wasm; sizes as WEB3D.md | `build.txt` |
| `three-reference.mjs --check` (three 0.180.0) | pass, 20 lines verbatim, the table matches | `three-reference-check.txt` |
| `demo/check.mjs` (twice) | exit 0: 64 draws, 320,000 triangles every run; 66/66 probes; steady 0.16 and 0 sampled B/frame; control 89.6 and 88.2 | `check.log`, `check/`, `check2.log`, `check2/` |
| `demo/check.mjs --blind` (twice) | exit 1, as it must: control 0.6 and 0.5 against 40 | `check-blind.log`, `blind/`, `check-blind2.log`, `blind2/` |
| every earlier critic's scripts (`rerun-earlier.sh`) | see below | `earlier/`, `rerun-earlier.log` |

The first pair's logs end in an empty "exit" line (the shell was zsh); `check2.log` and
`check-blind2.log` carry the exit codes.

## Earlier critics' probes

- First 29/29 (and its loss and wasm-edge scripts), second 14/14 and its ABI probe, third 10/10,
  seventh 23/23 (LC1-LC8, LC3b, D1, D2, T1-T5, S1-S3, H1, H2, W1, W2).
- Fourth: X1-X7 pass. Fifth: C1, C2, H5, R1, R2, T1, T2, L5, Y5, B1 pass. Sixth: G2-G7, G8b, G8c,
  G9a-c, G10, G11, G13, G16 pass (there is no G15).
- Fail by premise, exactly the seven named, with the details their explanations predict: X8 (fourth,
  fifth), K1 and E1 (fifth: E1 issues 134,213,633 handles, then "too many objects", first, middle and
  last refused, getError 0), G1, G8a (only `<img>` is refused), G12 and G14 (the counter wraps) of
  the sixth. Their intents pass in the seventh's H1, H2, S3 and T5, rerun here.

## This critic's probes (`run8.mjs`, `probe-site/`; `run8*.json`, `run8*.txt`)

| Probe | Result | What it shows |
| --- | --- | --- |
| E1 | **FAIL** | Renderer A disposed while lost (hook queued), then a create that fails as lost (hook queued). A's hook throws: the failed create's hook is never called (a 1, b 0). |
| E2 | pass | No task in which `isContextLost()` is false before `webglcontextrestored` is dispatched (Chrome flips both in one task), so `dispose()`'s `isContextLost()` test loses no hook. |
| E3 | pass | Restore hooks that create a renderer inside the hook: the first creates and draws, the second gets "already has a live renderer"; getError 0. |
| E4 | pass | Three renderers' hooks on one canvas (one live then disposed, two failed creates) each run once; one function queued twice runs once; the map entry is cleared. |
| E4b | note | One function given to a live renderer and to a failed create on its canvas runs twice on one restore. |
| E5 | pass | Disposed before or after the lost event: the hook comes once and a new renderer draws. |
| E6 | **FAIL** | A lost renderer left undisposed, then a new renderer on the restored canvas: on the next loss and restore, the old renderer's `onContextLost` and `onContextRestored` run again (2 each), beside the new one's. |
| E7 | pass | Chrome held 4 distinct errors at once (0x500, 0x502, 0x501, 0x506); the drain's bound of 16 covers them; the texture is made, with 4 "pending" warnings. |
| E7b | note | The page's own pending errors are consumed: its `getError()` after `createTexture` reads 0 (WEB3D.md says so). |
| E8 | pass | VideoFrames: Chrome uploads the visible rectangle, turned by the rotation; display sizes (smaller, larger, non-square) and rotations 90, 180, 270 all fill web3d's texture. |
| E9 | pass | A DataView over a sub-range, and another realm's DataView, compile and draw; so do SharedArrayBuffer-backed bytes. |
| E9b | note | A detached DataView is refused with V8's own TypeError, not a web3d message; the canvas is usable after. |
| E10 | pass | Right after `loseContext()`, before the event: `frame()` false, `resize` and creates throw "lost", the canvas keeps its size. |
| E11 | **FAIL** | A loss during `createRenderer`, on a canvas whose context the page made: the create throws "lost" and queues the hook, but nothing prevents the coming lost event's default, so Chrome refuses `restoreContext()` ("context restoration not allowed") and the hook never comes. |
| E12 | note | A canvas lost while no renderer holds it is never restorable; the create's message still promises `onContextRestored`. |
| E13 | pass | `bitmaprenderer` canvases larger than their bitmap are refused. |
| E14 | pass | `restoreContext()` inside the lost event's dispatch does not restore; after it, it does (WEB3D.md's claim). |
| E15 | pass | The documented recovery, two cycles in a row on one canvas. |
| E16 | **FAIL** | A `bitmaprenderer` canvas smaller than its bitmap (4x4 canvas, 8x8 bitmap; element and OffscreenCanvas) is accepted, getError 0, and the texture holds only the bitmap's top-left corner. |

## Findings, most severe first

1. **A home path in committed evidence** (hard gate). `web3d/evidence/phase1/critic3/pkg-before/loadngo_web3d.wasm`
   (blob `f73f253e`, from `284194b6`, still in HEAD's tree) holds three panic-location strings under the builder's
   home directory (its rustup toolchain's library sources; `strings -n 6` and a grep for the home prefix). This round redacted the text records only.
   Fix: remove the file (or replace it with its size and sha256). The branch's history also holds the
   unredacted text records: 34 text-file versions in `284194b6`, `59b74ced` and `ffe5f208` with home
   paths (`gates.txt`, the build and check logs, the `critic*/` run scripts, `wasm-bindgen-conflict.txt`);
   squash before any push.
2. **A throwing restore hook stops the hooks queued after it** (`js/web3d.js:573-576`; E1). The
   hooks of different renderers and failed creates on one canvas share one listener, and
   `for (const h of hooks) h();` has no guard, so one page bug (an old renderer's hook touching freed
   state) costs another create its only signal. It also breaks the comment's "Calls `hook` once when
   the canvas's context is restored" (`:555-560`). Fix: call each hook in its own `try`, report with
   `reportError` (or rethrow from a `setTimeout`), and go on.
3. **A `bitmaprenderer` canvas smaller than its bitmap is cropped silently, not refused** (E16).
   False now: `docs/WEB3D.md:93` ("A source whose pixels still do not fit the size it reports (a
   canvas with a `bitmaprenderer` context) is refused by the check after the upload"),
   `docs/WEB3D.md:198-200` ("`getError()` after the upload says whether the source's pixels fitted"),
   `js/web3d.d.ts:82-83`, `js/web3d.js:185-187`, `demo/probe.js:1048-1049`. Fix: say plainly that a
   `bitmaprenderer` canvas may be cropped or refused, and that the `ImageBitmap` itself is the source
   to pass; or refuse such canvases.
4. **A lost, undisposed renderer still hears the canvas's later losses and restores** (E6). Its
   shim's listeners stay until `dispose()` (`js/gl.js:94-102`), and WEB3D.md allows disposing "in
   `onContextLost` or later" and creating the next renderer beside it. Its hooks then run for the
   next renderer's context: with a recovery hook that creates, each later loss starts two creates, one
   of which fails "already has a live renderer". This contradicts `web3d.d.ts:43` ("Called when this
   renderer's context is lost"), `js/gl.js:7` ("finished for good") and the round's "lost renderers
   inert". Fix: once the shim has seen its loss and then a restore, remove its listeners (or call its
   hooks only while the renderer owns the canvas).
5. **WEB3D.md:181-182, "A context lost so recently that its event has not been dispatched yet is
   still allowed a restore", is false for a loss during the create on a page-made context** (E11).
   The branch at `js/web3d.js:480-484` throws `LOST_AT_CREATE` and queues the hook, but adds no
   `preventDefault` listener (only `shim.dispose()` does, `js/gl.js:356-358`, and no shim exists yet).
   Chrome then answers `restoreContext()` with INVALID_OPERATION, "context restoration not allowed",
   so the promised hook never comes. Fix: add the same one-shot listener there, or keep one permanent
   `preventDefault` listener on every canvas web3d has taken a context from (which would also cover
   E12).
6. **`js/web3d.d.ts:152`: `frame()` "false while the context is lost".** It is false from the loss
   on, for good, also after a restore (`lost` "stays true after a restore", two lines up). Fix the
   wording.

## Notes

- E12: a canvas lost while no renderer holds it is dead (nobody prevented the event); `LOST_AT_CREATE`
  still says "create the renderer again once it is restored (this call's onContextRestored is called
  then)". True only conditionally; a page waits forever. The permanent listener of finding 5 fixes it.
- E4b: the comment "each hook once however often it is asked for" (`js/web3d.js:559`) holds within
  `awaitRestore` only; a live renderer's hook and a failed create's hook, if the same function, run
  twice on one restore.
- E9b: a detached DataView fails with V8's message (`asBytes`, `js/web3d.js:361-364`).
- `afterLoss`'s `setTimeout` branch (restored while a failed create unwinds, `js/web3d.js:552`) is
  reached by no probe, the builder's or mine; Chrome's restore needs tasks that a create's awaits do
  not normally span.
- The drain is sound: bounded, before the upload, and documented as consuming the page's errors.
  `check.mjs` counts only INVALID_OPERATION warnings in the probe run, so a web3d error drained as
  "pending" there would pass the check.
- Probe ids repeat in `demo/probe.js` (two groups R, C, G, H, P; T3 is also the seventh critic's
  name for another probe). Polish.
- WEB3D.md's figures that I checked against their sources hold: the Results table and allocation
  figures (`rendered-check.json`), 74 tests, 43 imports and 28 exported functions (from the built
  wasm), sizes, the handle layout, block sizes, the timer ring of 8 and 30-frame limit, the shading
  constants, the demo's 320,000 triangles, the dolores table and fit (2.320 + 0.00455 per 1,000
  pixels, R² 0.985), the wasm-bindgen versions, the unsafe-code split, and CI's wasm32 step.
- Phase 1 has still no measurement of its present code and load on the hardware it is for; WEB3D.md
  says so.

## Is phase 1 done?

Not yet. Every part of the bar exists and every gate passes. Against it: a home path still committed
(the hard gate), two bugs in this round's own recovery code (E1, E6), one documented recovery path
that never gets its signal (E11), and three false claims about texture sources and `frame()`. All
are small fixes.
