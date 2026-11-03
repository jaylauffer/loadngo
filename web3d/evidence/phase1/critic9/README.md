# Ninth critic, web3d phase 1 (2026-11-03), generic evidence

Judged: loadngo `web3d-phase1` at `994f9bc8` (base `dev`). Chrome 156.0.8078.80 headless
(`channel: 'chrome'`, ANGLE over Metal, Apple M4 Pro); rustc 1.99.0, wasm-opt 133. The Mac's GPU was
claimed for this run and nothing else used it. Anything about the application this renderer is
built for is kept in that application's own repository. Verdict: **FAIL**.

## Gates (all rerun here)

| Gate | Result | File |
| --- | --- | --- |
| `cargo fmt --check` (workspace, and `-p loadngo-web3d`) | pass | `gates.txt` |
| clippy `-D warnings`: CI's macOS-row workspace command; CI's wasm32 step; `-p loadngo-web3d` on the host | pass | `gates.txt` |
| `cargo test --locked -p loadngo-web3d`, debug and release | 74 pass each | `gates.txt` |
| `build.sh` twice in place, once from an empty target dir | all four `pkg/` files byte-identical to the `pkg/` found (sha256); no local path in any of them (`strings`, `grep -a`); sizes as WEB3D.md | `build.txt` |
| `three-reference.mjs --check` (three 0.180.0) | pass, 20 lines verbatim | `three-reference-check.txt` |
| `demo/check.mjs` | exit 0: 64 draws, 320,000 triangles every run; 67/67 probes; pending GL errors 2 of 2 on purpose; steady 0.07 sampled B/frame; control 88.1 | `check.log`, `check/` |
| `demo/check.mjs --blind` | exit 1, as it must: control 0.7 against 40 | `check-blind.log`, `blind/` |
| every earlier critic's scripts (`rerun-earlier.sh`) | see below | `earlier/`, `rerun-earlier.log` |

Confidentiality: a whole-word grep of the HEAD tree under `web3d/`, `docs/WEB3D.md`, `.github` and
the root Cargo files (313 files, binaries with `grep -a` and `strings`) for the application, its cast,
clients and assets finds nothing; nor does it find a home path. The five commit messages are clean
too. Branch history still holds home paths in older versions of 16 to 27 evidence files (commits
`284194b6`, `59b74ced`, `ffe5f208`): squash before any push.

## Earlier critics' probes

First 29/29, second 14/14 (and its ABI probe), third 10/10. Fourth X1-X7; fifth C1, C2, H5, R1, R2,
T1, T2, L5, Y5, B1; sixth G2-G7, G8b, G8c, G9a-c, G10, G11, G13, G16; seventh all but LC5; eighth E1,
E2, E3, E5-E15 (E1, E6, E11 and E12, its findings, now pass). Exactly the by-premise failures that
WEB3D.md names fail, with the details its explanations give: X8 (fourth: only `o + 2^-30`, which is
`o`; fifth), K1 and E1 (fifth), G1, G8a, G12, G14 (sixth), LC5 (seventh: `secondLossPrevented`
true, by design), `manyOnOneCanvas` (E4, E4b: reads the old internal map) and E16 (eighth). Their
intents: this critic's N2 (several hooks on one canvas, one function twice, through two module
copies: each once) and N11 (`bitmaprenderer` canvases refused) pass.

## This critic's probes (`run9.mjs`, `probe-site/`; `run9*.json`)

`run9.mjs` also counts a canvas's event listeners over the DevTools protocol
(`DOMDebugger.getEventListeners`).

| Probe | Result | What it shows |
| --- | --- | --- |
| N1 | **FAIL** | The page's own `webglcontextrestored` listener, added before web3d first used the canvas, disposes the renderer that was live at the loss. Its `onContextLost` ran (1); its `onContextRestored` never does (0). |
| N1b | pass | The same in the page's own lost listener: the hook is queued and comes once. |
| N2 | pass | Three failed creates while lost (one function given twice, once through a second module copy) and the live renderer disposed: each hook once; a new renderer draws, `getError` 0; the canvas has exactly one lost and one restored listener before and after. |
| N3 | pass | A loss and restore both between two checks finish the old renderer; a new one takes the canvas while the old is undisposed; the old one's `dispose()` then leaves the new one drawing, `getError` 0. |
| N4 | pass | Disposed between `loseContext()` and the lost event: no `onContextLost`, `onContextRestored` once. |
| N5 | pass | Throwing hooks are reported and stop nothing; a create made inside `onContextLost` fails as lost and its hook runs once; a create inside that hook draws. |
| N6 | pass | Hooks that dispose another renderer (another canvas; the same canvas, old and new) and create a third. |
| N7a | pass | A page-made context lost, event not cancelled, before web3d: refused as lost before web3d, never restored, no hook (as documented). |
| N7b, N7b' | note | Lost in the same task as the create, its event still pending: the create says it "can be restored only if the page cancelled the lost event", but web3d's new listener cancels it, and the restore and hook come. |
| N7c | pass | Lost and cancelled by the page before web3d: the hook comes once, a new renderer draws. |
| N8 | **FAIL** | A page-made context that suits (depth, antialias as asked), with one piece of state the page left: scissor, colour mask, depth func, cull face, clear depth, depth range, rasterizer discard, draw buffers, or a framebuffer bound. The create succeeds and web3d draws nothing at the centre, in all nine; `getError` 0 in eight (the ninth: INVALID_ENUM from `invalidateFramebuffer`). Unpack skips or a bound `PIXEL_UNPACK_BUFFER` make `createTexture` fail, blaming the source. The control draws. |
| N9 | pass | Two module copies: one copy's live renderer refuses the other's create; hooks from both run once; the second copy's renderer hears the next loss and restore; one listener of each kind. |
| N10 | **FAIL** | A same-origin frame's copy first takes a top-document canvas's context, then the frame is removed. The top window's renderer on that canvas: the lost event is not cancelled (`defaultPrevented` false), `onContextLost` 0, `restoreContext()` refused ("context restoration not allowed"), no restore, `onContextRestored` 0; the next create says "the context is lost ... onContextRestored is called on the restore", which never comes. The two listeners are still there (DevTools count 1 + 1), but Chrome no longer runs a removed frame's listeners. |
| N10b | pass | A frame's renderer on the frame's own canvas loses and restores; hooks once; the top window's renderer is untouched. |
| N10c | pass | N10's control: with the frame kept, the loss is cancelled and the hooks run. |
| N10d | pass | A frame's copy made the shared state on the top window and the frame was removed: the top window's copy still creates, draws, recovers and refuses a second renderer. |
| N10e | **FAIL** | After N10's setup, a canvas web3d has used is never restorable again. |
| N11 | **FAIL** | `createTexture` on a canvas with no context gives it a `2d` context for good: then `getContext('webgl2')` and `getContext('bitmaprenderer')` return null, `transferControlToOffscreen()` throws, a later `getContext('2d', { alpha: false, willReadFrequently: true })` gets alpha true and willReadFrequently false, and `createRenderer` on it fails. An `OffscreenCanvas` likewise. A canvas with a WebGL context is untouched; a `bitmaprenderer` canvas of its bitmap's size is refused (by design). |
| N13 | pass | A worker's copy on a transferred `OffscreenCanvas`: draws, is lost, disposed while lost, gets `onContextRestored` once, a new renderer draws. |
| N21 | note | A detached `OffscreenCanvas` (transferred away) is refused as "its control was transferred to an OffscreenCanvas". |

**The two proposed fixes, tried** (`probe-site/fix/web3d.js`, a copy of `pkg/web3d.js`; `run9-fix.json`):
`dispose()` queues `onContextRestored` when `this._shim.lost()` instead of when
`this._gl.isContextLost()`, and `resetState(gl)` puts WebGL's default state back before `web3d_init`.
With them N1 and N8 pass, and N1b, N2-N6 and N9 still pass. Nothing in loadngo was changed.

## Findings, most severe first

1. **A page-made context is used with whatever state the page left, and web3d then draws nothing,
   silently** (N8). `js/web3d.js:507-551` takes the page's context as it is, and `src/renderer.rs:861-867`
   sets only the depth test and the blend function; scissor, colour mask, depth func and range, clear
   depth, cull face, rasterizer discard, draw buffers, the framebuffer binding and the unpack state are
   inherited. WEB3D.md:184-187 and :91 say a page-made context is refused only without depth or with
   another antialias. The upload failure's text (`js/gl.js:251`, "the source's pixels do not fit")
   blames the source. Fix: set that state to WebGL's defaults before `web3d_init` (14 calls; tried
   above), better in `Renderer::init` so the mock tests see it, and add a probe.
2. **The renderer live at the loss loses its `onContextRestored` when the page disposes it in a restored
   listener that runs before web3d's** (N1). `js/web3d.js:1147-1151` queues the hook only while
   `isContextLost()` is true, which is false once the context is restored, before the controller's
   listener has run. False now: WEB3D.md:219-221 ("calls `onContextRestored` for the renderer that was
   live at the loss"), `js/web3d.d.ts:48-53`, `js/web3d.js:586-588`. Fix: queue when `this._shim.lost()`;
   the controller clears `current` when it handles the restore, so a renderer still current and finished
   has not had its hook (tried above).
3. **A canvas whose controller was made by a removed frame's copy is never restorable again** (N10,
   N10e). `js/web3d.js:592-612` makes the listeners in whichever realm first takes the canvas's context
   and shares them, through the top window, with every copy; Chrome stops running a removed frame's
   listeners, so no loss is cancelled and no hook runs, for the canvas's life. False now: WEB3D.md:213-222
   ("keeps them for the canvas's life", "cancels the event, so the context can always be restored"),
   `js/web3d.js:30-33`, `:509-510`, `js/web3d.d.ts:51-52`. Fix: record the window that installed the
   listeners; when a create finds it closed, remove them and install this realm's, keeping `losses` and
   `waiting`; and add a probe.
4. **`createTexture` turns a context-less canvas into a `2d` canvas for good** (N11).
   `js/web3d.js:207-221` asks for `'2d'` to tell a `bitmaprenderer` canvas apart. WEB3D.md:93 and
   `js/web3d.js:190-192` present the effect as harmless ("uploads the same transparent pixels"); it also
   stops the canvas from ever taking a WebGL, WebGPU or `bitmaprenderer` context or being transferred, and
   makes later `2d` options be ignored. `js/web3d.d.ts:84-89` does not mention it. There is no side-effect-free
   way to ask a canvas whether it has a context, so the fix is to say so in all three places (and in
   probe TU), or to stop taking canvases with no context.

## Notes

- N7b': the `LOST_BEFORE_WEB3D` message (`js/web3d.js:53-54`) and WEB3D.md:226-227 are too pessimistic
  when the loss's event is still pending: web3d's listener comes first and cancels it.
- N21: `js/web3d.js:216-217` names the wrong cause for a detached `OffscreenCanvas`.
- WEB3D.md:329-331, precision: UVs, the world position and the material vector are `highp` in the
  fragment stage too (`src/shaders.rs:120-147`).
- WEB3D.md:407, "Each p95 is within 2.0% of its median": 2.02% at 1024x576 with MSAA (0.858 / 0.841).
- The figures I checked hold: the Results table, allocation figures, control 90.6 and blind 0.5
  (`rendered-check.json`, `blind-control/`), 74 tests, 43 imports and 28 exported functions (from the
  built wasm), sizes, the handle layout and table life, block sizes 144/128/32, the timer ring of 8, the
  shading constants, the 20 transcribed lines, three's `WebGLRenderer.js` lines 2501-2504, the dolores
  table and fit, the 67 unique probe ids, and the critics' table.
- The dolores figures are of an earlier load and shaders, as WEB3D.md says. The present demo has since
  been measured on dolores outside this repository; its generic figures could be recorded here.

## Is phase 1 done?

Not yet. Every part of the bar exists, every gate passes, every earlier probe passes or fails only by
its documented premise, and the controller holds under several renderers, failed creates, throwing and
creating hooks, losses between checks, two module copies and workers. Against it: page-made contexts
draw nothing with inherited state (1), one listener order loses the recovery signal (2), a removed
frame's realm leaves a canvas unrecoverable (3), and a texture check changes its source (4). Findings 1
and 2 are small code fixes, tried here; 3 is a small design change; 4 is documentation.
