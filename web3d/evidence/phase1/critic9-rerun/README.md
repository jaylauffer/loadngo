# The ninth critic's findings, fixed, and every critic's probes rerun (2026-11-03)

The tenth builder round, on loadngo `web3d-phase1` after `994f9bc8`, committed as `d924345f`. Chrome 156.0.8078.80 headless (`channel: 'chrome'`, ANGLE over Metal, Apple M4 Pro);
rustc 1.99.0, wasm-opt 133. The Mac's GPU was claimed for the runs.

## What changed

1. **A context is taken with WebGL 2's default state put back** (finding 1, N8). `Renderer::init`
   calls `reset_context_state` (`src/renderer.rs`) before anything else: it ends transform feedback
   and any running query, binds the canvas's own framebuffer for drawing and reading, puts back what
   `EXT_clip_control`, `EXT_depth_clamp` and `WEBGL_clip_cull_distance` add, switches off scissor,
   stencil, polygon offset, alpha-to-coverage, sample coverage and rasterizer discard (dithering on),
   and resets the colour mask, depth function, range and clear value, cull face, blend equation,
   hints, pixel unpack and pack buffers and parameters, vertex array, sampler objects and generic
   vertex attributes. The unit test `init_puts_back_webgl_default_state_before_anything_else` checks
   the calls in order. `WEBGL_polygon_mode` was left out, for two reasons the tenth critic found false
   (its F4): Chrome prints its warning once a page, not at every enable, and the mode can be told
   without enabling the extension, by a test draw. The eleventh round fills polygons when that test
   draw finds lines (`../critic10-rerun/README.md`). The upload failure's text no longer blames the
   source.
2. **`dispose()` queues `onContextRestored` when the renderer's shim is lost** (finding 2, N1), not
   when `isContextLost()` is true: the controller clears its live renderer only when it handles the
   restore, so a renderer still live and finished has not had its hook, even once the context is
   restored by a listener of the page's that ran first.
3. **Listeners follow the windows that can run them** (finding 3, N10, N10e). The listeners are made
   by the top window's copy of `web3d.js` once one has loaded, else by the copy of the latest create;
   each create puts them back from that copy when they are another copy's or the window that made
   them is gone (its `closed`, or a document that is no longer its own), keeping the loss count, the
   live renderer and the waiting hooks. A create after a loss no live listener saw says so
   (`LOST_UNWATCHED`). One case is left and documented: no copy in the top window, the frame whose
   copy made the listeners gone, and a loss before any create since.
4. **`createTexture` on a canvas with no context is documented plainly** (finding 4, N11), in
   `docs/WEB3D.md`, `js/web3d.js` and `js/web3d.d.ts`: a canvas must be drawn on before it is passed,
   and what happens to one with no context.
5. **The notes.** The message for a loss web3d's listeners did not see says both cases (N7b'); a
   detached `OffscreenCanvas` is named as detached (N21); `docs/WEB3D.md` gives every `highp` value
   of the fragment stage; the p95 sentence is computed from the new run.

## The new probes (`demo/probe.js`, run by `demo/check.mjs`)

| Probe | Finding | What it checks |
| --- | --- | --- |
| PS | N8 | 30 states a page can leave on a context it made (the ninth critic's twelve among them) and two controls: web3d draws, uploads and reads back as on a new context, with `getError` 0, and the page's objects are kept |
| LR | N1 | the page's own restored listener, added before web3d's, disposes the live renderer: `onContextRestored` once; the same in its lost listener; a dispose after the restore was handled adds no call |
| RF | N10, N10e | a same-origin frame's copy took a context from this document's canvas, then the frame was removed or navigated: with and without a copy in the top window, the next loss is cancelled and its hooks run once, the canvas keeps one listener of each kind (counted over the DevTools protocol); the case left is not cancelled and the create says why; a frame's renderer left live holds the canvas until a window holding it disposes it |
| NC | N11 | as documented: a canvas with no context uploads and has a `2d` context for good; one with a `2d` or WebGL context is unchanged; a `bitmaprenderer` one is refused and stays one |
| MS | N7b', N21 | the messages for a detached `OffscreenCanvas`, a placeholder canvas, and a loss before web3d's first use, pending or dispatched |

## The control: the new probes against the code the ninth critic judged

`build-before-fix.sh` builds `994f9bc8`'s web3d as a standalone crate; `probes-against.mjs` serves
the current `demo/` with that `pkg/` and runs the probe page (`before-fix.json`, summarised in
`before-fix.txt`). 68 of 72 pass: PS, LR, RF and MS fail, as they should. PS fails in 27 of its 30
states: every one of the ninth critic's, and every other one but three. Two the old code already
handled (a page's vertex array, and the WebGL flip and premultiply flags, which are set at each
upload), so they stand as regression guards. The third, `UNPACK_ROW_LENGTH` 1, guards nothing: as the
tenth critic found, and `../critic10-rerun/unpack-params/` measured, no row length changes an upload
from these sources; the eleventh round dropped it. LR shows `onContextRestored` 0 for
the renderer disposed in the page's restored listener. RF stops at its first case, the ninth critic's
N10: the loss is not cancelled, so the context is never restored. NC passes, since finding 4 is
documentation. With the fixes, all 72 pass (`../rendered-check.json`).

## Every critic's probes, rerun (`rerun-critics.sh`; `rerun-critics.log`, `earlier/`, `run9*.json`)

```
PLAYWRIGHT_CORE=.../playwright-core/index.mjs THREE_BUILD=.../three/build bash rerun-critics.sh <port>
```

**Critics one to eight** fail exactly the probes they failed in the ninth critic's own rerun of them
(`../critic9/earlier/`), each FAIL line the same, and each by the premise `docs/WEB3D.md` names: the
first 29 of 29, the second 14 of 14 (and its ABI probe), the third 10 of 10; the fourth's X8 (only
`o + 2^-30`, which is `o`); the fifth's X8, K1 and E1 (the earlier 30-bit handles); the sixth's G1,
G8a, G12 and G14; the seventh's LC5 (a later loss cancelled by design); the eighth's
`manyOnOneCanvas` (it reads the previous round's internal map) and E16 (`bitmaprenderer` canvases are
refused).

**The ninth critic's own**, against the code it judged and now:

| Probe | At `994f9bc8` | Now |
| --- | --- | --- |
| N1 | FAIL | pass: `onContextRestored` 1, `onContextLost` 1, the next renderer draws |
| N1b, N2-N6, N7a, N7c, N9, N10b, N10c, N10d, N13 | pass | pass |
| N8 | FAIL | pass: all nine draw states and three upload states draw or upload right, `getError` 0 |
| N10 | FAIL | pass: the loss is cancelled, both hooks once, one listener of each kind |
| N10e | FAIL | pass: restored, and the next create draws |
| N7b' | note (FAIL) | pass: the message no longer says only the page can cancel the lost event |
| N21 | note (FAIL) | pass: "this OffscreenCanvas cannot be read (it was transferred elsewhere, so it is detached)" |
| N7b | note (FAIL) | fails by its premises: the restore comes and the hook runs once, as before; but it wants a message that does not say the context was lost before web3d first used the canvas (it was), and its own lost listener, added before web3d's, to see the cancellation (`bPrevented`), which a listener that runs first cannot. Its intent is N7b' and probe MS |
| N11 | FAIL | fails by its premise: it asks that a canvas with no context be left able to take a WebGL context and be transferred. No browser call tells whether a canvas has a context without making one, so this is documented instead (`docs/WEB3D.md`, `createTexture`; `js/web3d.js`; `js/web3d.d.ts`), and probe NC checks the documented behaviour |

`fix.html` is not rerun: it tried the critic's patched copy of the previous `web3d.js`, whose shim
cannot load the present module (the module now imports 58 functions, not 43).
