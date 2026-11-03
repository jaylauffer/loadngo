# The tenth critic's findings, answered, and every critic's probes rerun (2026-11-04)

The eleventh builder round, on the working tree of loadngo `web3d-phase1` at `d924345f`. Chrome
156.0.8078.80 headless (`channel: 'chrome'`, ANGLE over Metal, Apple M4 Pro); rustc 1.99.0,
wasm-opt 133. The Mac's GPU was claimed for the runs, and the claim was shared with another session's builder
(Blender bakes on the CPU, look captures in headless Chrome); `../gates.txt`'s main rendered run was
quiet (GPU p95 within 2.9% of its median), after a wait for the other sessions' browsers to stop.

## The scope, and what changed

The integrator narrowed what web3d supports, rather than follow each new arrangement of frames: **a
canvas is driven by copies of web3d in the canvas's own realm**, a top-level page or one frame (or a
worker, for an `OffscreenCanvas` it holds). Cross-realm use of one canvas is unsupported: where it is
cheap to detect, it is refused.

1. **One realm drives a canvas** (findings F1 and F3). `canvasKind` (`js/web3d.js`) refuses a canvas
   whose prototype chain is not this realm's `HTMLCanvasElement` or `OffscreenCanvas` prototype,
   before any context is made or any listener added. That covers a same-origin frame's copy given a
   canvas of the top document (the case under F3, which can no longer arise in either load order),
   the top's copy given a frame's canvas, either given an `OffscreenCanvas` of the other, and F1's
   child frame given its parent's canvas under a top of another origin. The prototype check needs no
   mark on the canvas: a canvas of another realm is refused whether or not anyone drives it yet.
   With it, the machinery that moved listeners between realms (the installer the top window's copy
   registered, the liveness test, `LOST_UNWATCHED`) has no case left, and is gone: a canvas's two
   listeners are made once, by its own realm, as in the ninth round.
2. **The shared state's home** (F1's false claims). `HOME` is the top window if the copy can reach
   it, else the highest ancestor it reaches through parents of its own origin. So copies in a frame
   and its same-origin descendants under a top of another origin share one serial counter, which is
   what the documents now say (`docs/WEB3D.md`, `js/web3d.js`, `js/web3d.d.ts`, `src/slots.rs`,
   `src/web.rs`).
3. **Colour spaces and the drawing buffer** (F2). `reset_webgl_state` (`js/gl.js`), now called last
   in `Renderer::init`'s reset, sets `unpackColorSpace` and `drawingBufferColorSpace` to `'srgb'`,
   and, on a context with alpha whose drawing buffer is not `RGBA8`, calls
   `drawingBufferStorage(RGBA8, w, h)`. Without alpha, `drawingBufferStorage` raises
   `INVALID_OPERATION` ("alpha is required"), so no page can have changed that format, and web3d does
   not call it there (its first try did, and every web3d-made context showed the error).
4. **`WEBGL_polygon_mode`** (F4). After the rest of the reset, a test draw (one triangle far larger
   than a 1x1 framebuffer) tells whether polygons are drawn as lines; only then is the extension taken
   (the page's, already enabled) and `FILL_WEBGL` set, with `POLYGON_OFFSET_LINE_WEBGL` off. A page
   that never enabled the extension gets no warning from web3d: the probe page shows Chrome's warning
   once, from the PS case that enables it itself.
5. **Small claims** (F5): the shim is handed the context, `js/web3d.js` makes it; data textures are
   `RGBA8`; the ninth rerun's README no longer calls the `UNPACK_ROW_LENGTH` case a guard, and the
   case is gone from PS. `unpack-params/` measured why: no row length, image height, skip-images or
   alignment changes an upload from an `ImageBitmap`, `ImageData` or 2d canvas here (21 cases, all as
   the control).
6. **Notes.** WEB3D.md marks inferences as inferences; the CPU-timing figure is measured again
   (`cpu-timing/`); `gates.sh` judges every gate by its exit status (the blind run must fail),
   names the ones that did not hold, and exits 1 then; its header names the commit and the changed
   paths; `transferControlToOffscreen()` is in the "no call tells" sentence; `build.sh`'s workspace
   remap is described as changing nothing today.

## The probes (`demo/probe.js`, run by `demo/check.mjs`)

- **XR**, new, in place of RF (whose cross-realm cases are now refused): a same-origin frame's copy is
  refused a canvas of this document and an `OffscreenCanvas` made here; this window's copy the frame's
  canvas and the frame's `OffscreenCanvas`; each refused canvas keeps no context and no listener
  (counted over the DevTools protocol); each copy draws on a canvas of its own. Under a top of another
  origin (`demo/probe-xorigin.html`, served from `localhost` while the probe page is `127.0.0.1`; the
  servers send `Cross-Origin-Resource-Policy: cross-origin` so the isolated page may embed it): the
  child's copy, loaded before its parent's, is refused the parent's canvas before and after the parent
  draws there; the two copies share one home (the child's state object is the parent's, and their
  serials follow one another); the parent is refused the child's canvas; and the parent, once the
  child has gone, recovers from a loss with each hook once.
- **PS**: 36 states and two controls. New: `unpackColorSpace` `'display-p3'` with an `ImageBitmap`,
  an `ImageData` and a 2d canvas upload, each read exactly; `drawingBufferStorage` `SRGB8_ALPHA8` and
  `RGBA16F`, read exactly, and the format `RGBA8` afterwards; `drawingBufferColorSpace` `'display-p3'`
  (the same bytes, so the check is the property afterwards); `WEBGL_polygon_mode` lines with
  `POLYGON_OFFSET_LINE` on, read off the quad's diagonal (which lines would draw too), and the mode
  `FILL` afterwards.

## The control: the new probes against the code the tenth critic judged

`build-before-fix.sh` builds `d924345f` as a standalone crate; `probes-against.mjs` serves the current
`demo/` with that `pkg/` (`before-fix.json`). 70 of 72 pass: PS and XR fail, as they should.

- PS fails in the seven new states: the three `display-p3` uploads read (239, 76, 67), (129, 255, 94)
  and (56, 56, 250) for red, green and blue texels against (255, 56, 56), (56, 255, 56) and
  (56, 56, 255); the sRGB drawing buffer reads (129, 255, 129) against (56, 255, 56); the float one
  stays float, so the probe's byte read-back raises `INVALID_OPERATION` (the tenth critic's float
  read-back shows web3d drew it right, 134 like its control: a float buffer changes how a page must
  read it, not what web3d draws); `drawingBufferColorSpace` stays `'display-p3'`; and the quad is
  drawn as lines, black off its diagonal.
- XR: every cross-realm create succeeds, including the frame's on this document's canvas, which gets
  web3d's two listeners; under the top of another origin the child's copy has a home of its own
  (`childHomeIsThisWindow` false; serials 560139 and 585108, from two random counters).

With the fixes, all 72 pass (`../rendered-check.json`, from `../gates.sh`, in which every gate held).

## The CPU timer's cost, measured again (`cpu-timing/`)

The first measurement (`../cpu-timing-allocation/`) no longer runs: its page asks V8 about
`_putMatrix`, now `_put`. `cpu-timing/` is a copy for today's code, with one addition, `held=`,
which says where the page keeps the handle it passes to `setTransform` each frame.
`runs-40s.txt` (3,000 frames after 40 s of frames, one headless Chrome a run):

| Page | Sampled JS bytes a frame |
| --- | --- |
| handle in a constant, timing off | 0 (only the measuring call's 1.5) |
| handle in a constant, timing on | 22.0 |
| handle in an Array (the original page), timing off | 12.0 |
| handle in an Array, timing on | 34.2 |
| handle in an object property, timing off | 11.3 |
| handle in a `let` variable, timing off (`runs-40s-let.txt`) | 0 (only the measuring call's 1.1) |

So the timer costs 22.0 (20.8 in the first round). And a second cost the first measurement could not
show, since handles had 30 bits then, small integers V8 stores as they are: a handle is at least
2³⁰ now, so V8 boxes it (12 bytes) when the page reads it from an Array or an object property to
pass it. `runs-120s.txt` (the
original page, 7,800 frames after 120 s) shows the same 12 at the tick, with `_put` still at V8's
middle tier. A run with TurboFan inlining off charged it to the tick too, not to web3d's methods.
`docs/WEB3D.md` ("No per-frame allocation") says so, and the documents no longer say that nothing on
the per-frame path allocates without saying where.

## Every critic's probes, rerun (`rerun-critics.sh`; `rerun-critics.log`, `earlier/`, `run9*.json`, `run10-*.json`)

```
PLAYWRIGHT_CORE=.../playwright-core/index.mjs THREE_BUILD=.../three/build bash rerun-critics.sh <port>
```

The tenth critic's pages run through `run10-copy.mjs`, its driver with one line changed, so that it
reads `../critic10/site/` and writes its results here, not over the critic's own.

**Critics one to eight**: every FAIL line is the same as in the ninth rerun (`../critic9-rerun/earlier/`),
each by the premise `docs/WEB3D.md` names: the fourth's X8; the fifth's X8, K1 and E1; the sixth's
G1, G8a, G12 and G14; the seventh's LC5; the eighth's `manyOnOneCanvas` and E16.

**The ninth's**: N1-N6, N7a, N7c, N8, N9, N10b, N10d, N13, N7b' and N21 pass. N7b and N11 fail by
the premises the ninth rerun gave. N10, N10c and N10e now fail by the scope decision: each has a
same-origin frame's copy take a canvas of the top document, which is refused ("this canvas belongs
to another realm"); probe XR checks that refusal.

**The tenth's** (`run10-*.json` against `../critic10/run10-*.json`):

- `state`: its 23 added states draw right, as before. Its six colour-space and drawing-buffer cases
  now read as their controls: the P3 upload (206, 114, 76), the sRGB and float buffers and the P3
  colour space 134, against (194, 118, 84), 192, black with `INVALID_OPERATION`, and 134 at
  `d924345f`. Its pass test for those six is a green pixel, which their own controls fail too.
- `state2`: every F2 and F4 case reads as its control: lines filled (green off the diagonal), each P3
  upload (206, 114, 76), the sRGB buffer 134, and the P3 drawing buffer's screenshot (209, 111, 69),
  with the property `'srgb'` afterwards. Except `storageFloat`, by premise: it reads the drawing
  buffer as floats, and web3d has put it back to `RGBA8`, so that read raises `INVALID_OPERATION`.
- `pm`: Chrome's facts, as before (getParameter refused where the extension is not enabled; the
  test draw tells lines from fill). Its two console warnings are its own calls'.
- `notop`: refused at its first create, a frame's copy on a canvas of the top document (the scope
  decision), so R1 to R4 do not run.
- `xtop`: the second renderer on the parent's canvas is refused, which was F1's bug. Its
  `sharedStateSame` is false by premise: it looks for the child's state on the child's own window,
  where a home's state is not kept (XR checks it on the parent, from the child).
- `bf`: holds, as before.
