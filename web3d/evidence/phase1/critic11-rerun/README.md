# The eleventh critic's findings, answered, and every critic's probes rerun (2026-11-04)

The twelfth builder round, on the working tree of loadngo `web3d-phase1` at `33df5d4a`. Chrome
156.0.8078.80 headless (`channel: 'chrome'`, ANGLE over Metal, Apple M4 Pro); rustc 1.99.0,
wasm-opt 133. The Mac's GPU was claimed for the runs, shared with other sessions, whose headless
Chromes drew during most of this round's runs; `gates-contention.txt` records what was running
during the timed gate.

The supported model is unchanged: one realm drives a canvas, and a canvas of another realm is
refused before any context or listener is made. Only the check was wrong.

## What changed

### F1: an element is judged by its document, not its prototype

`canvasKind` (`js/web3d.js`) now asks two different questions for the two kinds of canvas:

- **An element** is taken only when its owner document is this window's `document`. The owner
  document is read through the built-in `Node.prototype.ownerDocument` getter, taken when the module
  loads, and compared with the window's live, unforgeable `document`.
- **An `OffscreenCanvas`** keeps the prototype check. It is wrapped only where it is made or
  received by transfer, and it moves only by transfer, which detaches it.

The prototype never told an element's realm. These measurements show why (`results/facts.json`,
`facts.html`, raw WebGL, no web3d):

- the eleventh critic's adoption and first-touch cases give the frame's prototype to a canvas whose
  owner document is the top's;
- the `ownerDocument` getter of either realm answers the top's document for it;
- a WebGL context taken from such a canvas has the frame's prototype too, its extension objects
  likewise. After the frame is removed, its methods still draw and read back, and this window's
  listeners cancel its loss and see its restore.

So the top's copy can drive such a canvas, and does now.

**A driven canvas moved to another document is unsupported, and refused.** Its listeners are this
window's. Moved into a frame that is then removed, the next lost event is not dispatched at all
(`../critic11/results/realm.json`, A8: `lostEvents` 0, even for a listener of the top's), so nothing
can cancel it. So `frame()`, `resize()` and the creates throw "moved to another document" while
the canvas is elsewhere, and work again once it is back (`Renderer._inPlace`: one getter call and
one property read; the rendered check's allocation gate runs through `frame()`). `dispose()` still
works.

The documents are corrected to match:

- `docs/WEB3D.md`: the realm section, rewritten; the loss controller, which said the listeners "run
  as long as the canvas's document or worker does", now true by the refusal of moved canvases;
- `js/web3d.js`: `canvasKind` and `controllerFor`'s comments;
- `js/web3d.d.ts`: `createRenderer` and `frame`;
- the messages: `FOREIGN` is now two, one for an element ("in another document than this
  window's") and one for an `OffscreenCanvas` ("belongs to another realm"), plus `MOVED`.

### F2: one home a realm

The realm's first copy chooses its home and keeps it on the realm's global (`REALM_STATE.home`);
every later copy in that realm takes it. So a copy loaded after `document.domain` has made the top
reachable shares the first copy's owners and serial counter:

- the critic's `dd.html`: the second copy's create is refused with "already has a live renderer";
- this round's `site/dd.html` adds that the two copies' serials follow one another and the canvas
  has one controller, two listeners (`results/dd.json`). At `33df5d4a` it has two controllers and
  two counters (`control/dd-at-33df5d4a.json`).

**Where I departed from the suggested fix.** The critic suggested keeping owners, contexts and
controllers on `globalThis`, with only the counter on the home. I did that for the loss
controllers, which must be the driving realm's own functions. I kept the owners (and the antialias
cache, with them) on the home, for one case the suggestion would open:

- a canvas element moved into a same-origin frame's document while this window's renderer drives
  it;
- with owners per realm, the frame's copy would see no owner, take the canvas and share its context
  with the live renderer;
- moved back, this window's renderer would then draw with a stale state cache and no error.

With owners on the home, the frame's copy is refused with "already has a live renderer" (probe XD,
`moved.frameCopy`). Fixing the home per realm already closes F2, so the departure costs nothing
there. What remains is two realms with different homes that can reach each other: possible only
through `document.domain`, set after one of them loaded. A canvas moved between those two while
driven could get a second renderer. That needs `document.domain` and an unsupported move together.
WEB3D.md ("What copies share") lists what is shared where, and copies with different homes are in
none of its lists.

### F3: the blind run has its own exit code

`check.mjs --blind` exits:

- **3** when the positive control was not seen and everything else held, the one failure a blind
  run should have;
- **1** for anything else failing, a port taken or Chrome missing included (an exception before the
  browser starts also exits 1);
- **0** when even the blind profiler saw the control, which `gates.sh` also refuses.

`gates.sh` requires exactly 3 (`step_blind`). Its header says it does not run the critics' scripts.

### F4: a page's drawing-buffer size is not kept

Once `createRenderer` has sized the canvas, a context with alpha whose drawing buffer is not the
canvas's size has the canvas's width set to another value and back. The browser then makes the
buffer again at the canvas's size, within its own limits, keeping its format and samples. The reset
stores `RGBA8` at that size, as before. `facts.json` measured the details:

- Chrome keeps a stored size when the canvas is set to the size it has;
- a width set to another value and back reallocates, keeping `SRGB8_ALPHA8` and 4 samples;
- `drawingBufferStorage` over `MAX_RENDERBUFFER_SIZE` raises `INVALID_VALUE`, while a canvas over it
  is clamped. That is why web3d leaves the size to the browser's own resize rather than storing the
  canvas's size: a 20,000x20,000 canvas gets 5,760x5,760 here.

A context without alpha, web3d's own included, cannot have a stored size and is sized once. Probe DB
counts the width sets.

### F5: the false and stale claims

Every item of `../critic11/doc-check/README.md`, and of the README's F4 list. Each corrected claim
was checked against its source.

| Item | Was | Now | Checked against |
| --- | --- | --- | --- |
| 1 | three r180 sets both colour spaces "on every context it uses" (WEB3D.md, `gl.js`) | its `outputColorSpace` setter does, not its constructor | `three/src/renderers/WebGLRenderer.js` 276, 3399-3405 |
| 2 | the page's objects are "only unbound" (WEB3D.md, `renderer.rs`, `web3d.js`, `probe.js`) | which bindings are cleared, and which stay until the renderer binds its own | `reset_context_state`, `reset_webgl_state`, `drawsLines` |
| 3 | another renderer's handle is refused "with an error that says which" | names the kind of a wrong-kind handle; otherwise "destroyed, never created, or another renderer's" | `renderer.rs` `Error::Handle` |
| 4 | line width "which no draw uses" | the reset sets it to 1 before the test draw, which may draw lines; the blend colour alone is left | `gl.js` `reset_webgl_state` |
| 5 | the unpack parameters measured on 3 of the 6 source kinds | all six: 42 cases at `994f9bc8`, which does not reset them, and 42 now, all exact | `unpack-params/` |
| 6 | the tenth's `state`: "23 states draw right" | 22, and `EXT_blend_func_extended` skipped | `run10-state.json` (cases 1-23; 20 skipped) |
| 7 | `gates.sh` "runs every gate above" | runs the gates, not the critics' scripts | `gates.sh` |
| 8, 9 | 20.8 bytes in `check.mjs`, and "the documents quote the 20.8" | 22, and "quoted until the eleventh round" | `../critic10-rerun/cpu-timing/runs-40s.txt` |
| 10 | serials "560139 and 585108" | 33979 and 7064 | `../critic10-rerun/before-fix.json` |
| 11 | "a run with TurboFan inlining off" | not recorded then; run now | `handle-boxing/runs-40s-no-turbo-inlining.txt` |
| 12 | `_put` "still at V8's middle tier" | status 41, TurboFan | `../critic11/handle-boxing/runs-40s.txt` (forced references: 41 TurboFan, 25 Maglev) |
| 13 | `demo.js`: "nothing on the per-frame path allocates" | with CPU timing off; on by default for the readout | `demo.js` 36 |
| 14 | `index.html` lists six query parameters | all nine | `demo.js` |
| 15 | `web.rs`: "a frame allocates nothing on either side" | except the two boxed clock readings with CPU timing on | `gl.js` `now` |
| 16 | `frame()` false by "the flag the context-lost event sets" | `isContextLost()` or the controller's loss count, whichever first | `gl.js` `gone` |
| 17 | `gl.js`: "a cross-origin image throws" (and the same in `Error::TextureUpload`) | a tainted canvas or a cross-origin `ImageBitmap` | `imageSize` refuses `<img>` first |
| 18 | "the highest same-origin ancestor" (`web3d.js`, `web3d.d.ts`, `slots.rs`) | the top if reachable, else the last of the chain of same-origin parents | `REALM_STATE.home` |
| 19 | the probes' contexts released "so Chrome's limit drops none" (18 were dropped) | true now, and gated (below) | `../rendered-check.json`, `contextsLostToTheLimit` 0 |
| 20 | XR's "no listener" counted on one canvas | on all four refused canvases, each in its own frame's context | probe XR |
| polish | `setAmbient(sky, ground, intensity)`; two JSDoc blocks on `baseColor`; no constructor in the d.ts; "every method throws" after dispose | seven numbers; one block; `private constructor()`; the getters named | `tsc` 6.0.3 on a use of the d.ts (`new Renderer()` an error) |

### The notes

- **Line width.** `reset_webgl_state` sets `gl.lineWidth(1)` before the polygon-mode test draw. PS
  has a case of lines 8 wide, which passes with the width 1 afterwards. Metal clamps lines to 1, so
  the misreading itself cannot be shown here. The triangle's edges pass at least 1.4 pixels from the
  test pixel's centre (2 along x or y), as `gl.js` now says, not 2 everywhere.
- **Handle boxing**, stated exactly in WEB3D.md ("No per-frame allocation"), from the critic's
  measurement and this round's rerun of it (`handle-boxing/`). Phase 2's plan now says that
  per-frame calls should take handle objects or a renderer-owned per-frame typed array, not one
  number of 2^30 or more per object.

## The probes (`demo/probe.js`, run by `demo/check.mjs`): 74, all pass

- **XD**, new: an element belongs to its document's window whatever its prototype says.
  - A canvas the frame's document made and this one adopted (the frame's prototype), and one this
    document's parser made that the frame's `MutationObserver` touched first (the frame's
    prototype): the frame's copy is refused each, with no listener left, and this window's copy
    draws on each.
  - A canvas this document made, adopted by the frame's: this window's copy is refused it, and the
    frame's copy draws.
  - Then the frame is removed. This window's renderers on the two canvases with the frame's
    prototype still draw. Each loss is cancelled and restored, each hook comes once, and a new
    renderer draws.
  - A driven canvas moved into the frame's document: `frame()`, `resize()` and `createMaterial`
    throw "moved", and the frame's copy is refused ("already has a live renderer"). Moved back, it
    draws. Disposed and handed over, the frame's copy draws, and the canvas has a controller from
    each window (2 and 2 listeners).
  - Canvases of a `<template>` and a `DOMParser` document: refused, untouched, no listener.
- **DB**, new: four stored drawing buffers (sRGB and RGBA8, multisampled or not, 8x4 on 16x16 and
  40x30 on 300x150) end at the canvas's size, `RGBA8`, with their samples, drawn to the corner, and
  still after `resize`. Each takes three width sets. Two controls: a page context with alpha and
  nothing stored, and web3d's own context, take one.
- **PS**: 37 states and two controls. New: polygon mode lines 8 wide.
- **XR**: listeners counted on all four refused canvases (two elements, two `OffscreenCanvas`es),
  and the element and `OffscreenCanvas` refusals told apart by their messages.
- **Counting listeners.** DevTools lists an object's listeners only when the object is resolved in
  its own frame's context. A node of a frame's document, or an `OffscreenCanvas` a frame made, reads
  as having none from the top's, even with listeners on it (`results/listeners.json`, with
  controls). So `check.mjs`'s `window.__listeners` resolves the object in every frame's context and
  takes the longest list. Before, a refused frame canvas's "no listener" could not have failed.
- **Chrome's limit on live contexts.** The probe page tracks every context a probe makes (through
  web3d, or itself) and releases them once the probe is done. The run logs no "Too many active
  WebGL contexts" (it logged 18 before), and `check.mjs` now fails if it does.

### The control: the new probes against the code the eleventh critic judged

`../critic10-rerun/build-before-fix.sh <dir> 33df5d4a` builds `33df5d4a` as a standalone crate
outside the repository. `tools/against.mjs` serves today's pages with that `pkg/` (`control/`).

- `probes-at-33df5d4a.json`: 70 of 74 pass.
  - PS fails on the line width (8 kept).
  - XD fails on every F1 case: the frame's copy takes the adopted and first-touched canvases, the
    top's copy is refused them, and a moved canvas still draws.
  - DB fails on all four stored sizes (8x4 and 40x30 kept).
  - XR fails only on its messages: today's probe tells the element refusal from the
    `OffscreenCanvas` one by wording that the old code does not have.
- `intent-at-33df5d4a.json`: the frame's copy is not refused in every arrangement, the top's copy
  is refused canvases of its own document, and after the frame goes no loss recovers.
- `dd-at-33df5d4a.json`: two live renderers on one canvas, two counters, two controllers.

## Every critic's probes, rerun (`rerun-critics.sh`; `rerun-critics.log`, `earlier/`, `run9*.json`, `run10-*.json`, `results/`)

```
PLAYWRIGHT_CORE=.../playwright-core/index.mjs THREE_BUILD=.../three/build bash rerun-critics.sh <port>
```

All 34 runs exited 0: the 24 scripts and pages of critics one to ten, the eleventh's six pages, and
this round's four (`earlier/*.txt`; `round12-intent.txt` is a later run of the intent page, after its
N10c control was added). Against the eleventh critic's own rerun of critics one to
ten (`../critic11/rerun/`), `../critic11/tools/bool-diff.mjs` finds no boolean that differs in any
of their 21 result files, but two details of the seventh's S1: `ownShared` is now true, because a
frame's own window now holds its realm's state (its loss controllers and its home). S1 passes.

**Critics one to eight**: every FAIL line is the one the earlier reruns argued by premise, each
premise unchanged by this round:

- **the fourth's X8**: it refuses 10 of 11 values; the eleventh is the handle itself, since
  `o + 2^-30` rounds to `o` below 2^50;
- **the fifth's**:
  - X8: its fraction rounds to the handle, as the fourth's;
  - K1: it expects a 32-renderer limit, which the 20-bit serials no longer have;
  - E1: it expects the 30-bit table's 32,767 x 128 handles; the table now issues 134,213,633, then
    refuses cleanly;
- **the sixth's**: G1, the old handle layout (now below 2^50); G12, 16,384 values a slot (now
  2,048); G8a, `<img>` uploaded (now refused); G14, a counter that cannot go round (it goes round);
- **the seventh's LC5**: it holds but for one detail, a later loss that the loss controller now
  cancels by design;
- **the eighth's**: `manyOnOneCanvas` reads the previous round's internal map of waiting hooks,
  which the loss controller replaced; E16 expects `bitmaprenderer` canvases uploaded, which are now
  refused.

The eleventh critic retested the intents of `manyOnOneCanvas` and G14 without the old internals
(`../critic11/site/intent.html`). Both pass again (`results/critic11-intent.json`).

**The ninth's**: N1-N6, N7a, N7c, N8, N9, N10b, N10d, N13, N7b' and N21 pass. Two fail by the
premises the ninth rerun gave:

- N7b wants a message that does not say the context was lost before web3d first used the canvas,
  which it was, and wants its own earlier listener to see web3d's cancellation (its restore and hook
  pass);
- N11 wants a canvas with no context left as it was, which no browser call allows, so that is
  documented (`createTexture`).

N10, N10c and N10e fail literally: each has a same-origin frame's copy take a canvas of the top
document, which is refused.

**By their intent** (`site/intent.html`, `results/intent.json`). Their intent, and the tenth's
`notop`'s: a frame's copy must not drive a canvas of the top document, so that removing the frame
cannot leave that canvas with a loss nobody cancels. The page tries it in each arrangement that
gives a top-document canvas the frame's prototype, with the top loading web3d only after the frame's
copy has run:

- the frame's copy is refused the plain, adopted and first-touched canvases;
- the frame's copy recovers on its own canvas while the frame is kept (N10c);
- with the frame kept, the top's copy draws on and recovers each of the three (N10c);
- after the frame is removed, each recovers twice, with each hook once and a new renderer drawing
  after (N10, N10e);
- the top loaded web3d lazily (`notop`).

All pass. At `33df5d4a` the first, third and fourth fail (`control/intent-at-33df5d4a.json`).

**The tenth's** (`run10-*.json`): as in the critic's rerun.

- `state`: 22 of its 23 added states draw right, and `EXT_blend_func_extended` is skipped. Its
  colour-space and drawing-buffer cases read as their controls.
- `state2`: as its controls, but for `storageFloat`, by premise: it reads the buffer as floats, and
  web3d has put it back to `RGBA8`.
- `pm` and `bf`: hold.
- `notop`: refused at its first create, a frame's copy on a canvas of the top document. Its intent
  is retested above.
- `xtop`: its second renderer is refused; its `sharedStateSame` is false by premise (it looks for a
  home's state on the child's window).

**The eleventh's pages** (`results/critic11-*.json`, through `tools/run.mjs`, a copy of the critic's
driver that writes here):

- `realm`:
  - A1 and A3 are now accepted by the canvas's own document's window and refused by the other's.
  - A2 cannot be set up: the frame's copy is refused the adopted canvas. XD and the intent page run
    its intent instead.
  - A5 and A5b: accepted from the canvas's document's window, refused from the other.
  - A6, a frame canvas with this window's prototype set on it, is now refused too: the document
    decides, whatever the prototype.
  - A7's `sharesHome` reads false by premise: it expects the frame's window to hold no web3d state,
    and now it holds its realm's controllers and home. Its home is still the top: owners and serials
    are on the top's state.
  - A8 throws "moved" at its first draw after the move, by design.
  - A9: each copy is refused the other window's canvases, whichever window made them.
  - A10: holds.
- `observe`: this window's copy takes its parser-made canvas despite the frame's prototype, and the
  frame's copy is refused it.
- `dd`: the second copy is refused.
- `reset`:
  - S5 is 300x150 after create and after resize;
  - S2's line width is 1 after create, for every width set;
  - S3, S4, S6, S7 and S8 as before.
- `nowarn`: holds.
- `intent`: both pass.

## Browser facts this round rests on (`results/`)

- `facts.json` (`site/facts.html`): wrappers, `ownerDocument` and contexts across realms; canvases
  of template and `DOMParser` documents (WebGL works on them, so web3d refuses them by document, not
  by failure); the drawing-buffer sizes above.
- `listeners.json` (`site/listeners.html`): which listener counts DevTools can see from which
  context, with positive controls.

## Handle boxing, measured again (`handle-boxing/`)

`run.mjs` is the eleventh critic's driver (`../critic11/handle-boxing/run.mjs`), copied so that it
serves that folder's page with this round's `pkg/` and adds `JS_FLAGS` to Chrome's `--js-flags`.
It samples 3,000 frames after 40 s, one headless Chrome a run. `frame()` now makes one more getter
call a frame (the document check), and the figures match the critic's within a byte:

| Where the page keeps the handle | Critic 11 | Here (`runs-40s.txt`) |
| --- | --- | --- |
| an Array of numbers | 12.6 | 11.8 |
| an object property | 12.2 | 11.2 |
| a `Float64Array` | 11.9 | 12.1 |
| an Array that also holds a non-number | 0 | 0 |
| a `Map` value | 0 | 0 |
| a constant | 0 | 0 |
| 60 handles from an Array | 748.2 | 737.1 |

The 0s leave out the measuring call's 1.0 to 1.4. In every run `_put` is at status 41 and the
forced TurboFan reference reads 41, the Maglev one 25; with 60 handles every function is at 41.

`runs-40s-no-turbo-inlining.txt` is the run the tenth rerun's README described but never recorded:
`JS_FLAGS=--no-turbo-inlining`. From an Array, 11.4, charged to the page's function; from a
constant, 0. So the cost is the caller's, with inlining on or off, never web3d's methods'.

## Gates

`../gates.sh 5669 5670`, recorded in `../gates.txt` (home written `<home>`), exited 0: every gate
held.

| Gate | Result |
| --- | --- |
| `cargo fmt --check` | exit 0 |
| clippy, CI's macOS row (the workspace, `-D warnings`) and its wasm32 step | exit 0, exit 0 |
| clippy `-p loadngo-web3d --all-features` on wasm32, aarch64 Linux, Windows MSVC and the host | exit 0 each |
| `cargo test --locked -p loadngo-web3d`, debug and release | 75 passed each |
| `build.sh` x3, the third from an empty target directory | byte-identical `pkg/`, no local path |
| `three-reference.mjs --check` | 20 lines verbatim, output matches |
| the CI step's YAML | parses, the step is there |
| `check.mjs` | exit 0 |
| `check.mjs --blind` | exit 3, as required |

The builds:

- wasm 70,249 bytes (32,174 gzipped), `sha256 4d6594b5…`. That is 32 bytes more than `33df5d4a`'s,
  from the corrected `Error::TextureUpload` message; the comment edits move panic line numbers too,
  so the hash changes;
- `gl.js` 21,824 bytes (7,391 gzipped);
- `web3d.js` 56,597 bytes (17,636 gzipped).

The rendered check (`../rendered-check.json`, `../rendered-check.log`):

- 64 draws and 320,000 triangles in every run, the clear colour exact, the centre lit, `getError` 0,
  no page error;
- 74 of 74 probes, no `INVALID_OPERATION`, the two pending errors raised on purpose, and 0 contexts
  lost to Chrome's limit;
- steady allocation 0.09 sampled bytes a frame: 56 over 601 frames, all in the check's measuring
  call `window.__web3d.frames`, none in `pkg/`;
- the positive control 89.7;
- cold: 255.8 (the module's call wrappers), 29.8 (`web3d.js`), 2.2 (`gl.js`).

The blind run (`../blind-control/`): the control read 0.6, everything else held, exit 3.

**Contended.** The GPU was shared during the whole gate run: two other sessions' headless Chromes
were drawing (`gates-contention.txt`). In the 20 minutes before, checked by hand from 02:17 and every
5 s from 02:29, there was always at least one. So this run's GPU times are not capacity figures:

- GPU p50 0.48, 2.407 and 0.323 ms, and p95 6.118, 4.796 and 7.281 ms (low with MSAA, full, low
  without MSAA);
- CPU p50 0.055, 0.05 and 0.065 ms, and p95 0.07, 0.065 and 0.085 ms;
- every frame took 16.67 ms.

WEB3D.md's table keeps the last quiet run (`33df5d4a`'s committed `rendered-check.json`), with the
reason: nothing on the GPU's path changed.

**The confidentiality scan** (kept outside this repository, since its pattern names what it looks
for), over every file the branch changes from `dev` and every untracked file, this folder and
`../critic11/` included: zero hits on every line.
