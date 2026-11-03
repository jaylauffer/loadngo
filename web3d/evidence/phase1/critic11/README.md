# Critic 11: phase 1 at `33df5d4a`

Loadngo branch `web3d-phase1`, HEAD `33df5d4a`, clean tree. Chrome 156.0.8078.80 headless
(`channel: 'chrome'`, ANGLE over Metal, Apple M4 Pro); rustc 1.99.0, wasm-opt 133. Two other sessions'
headless Chromes were drawing during most runs, so every GPU timing here is contended.

**Verdict: FAIL.** The realm check does not test what the docs say it tests. A frame's copy can still
drive a canvas of the top document, and the tenth critic's F1/F3 failure comes back: a lost context
that can never be restored. The top's own copy can also refuse a canvas of its own document.

## Gates (`gates/`)

| Gate | Result |
| --- | --- |
| `cargo fmt --check` | exit 0 |
| clippy, CI's macOS row (workspace, `-D warnings`) | exit 0 |
| clippy wasm32, CI's step (and `--all-features`), aarch64 Linux | exit 0 |
| `cargo test -p loadngo-web3d`, debug and release | 75 passed each |
| `build.sh` x3, the third from an empty target dir | byte-identical, and the same hashes as `../gates.txt`; no local path (`builds.txt`) |
| `three-reference.mjs --check` | 20 lines verbatim, output matches |
| `check.mjs` | exit 0: 72 probes, steady allocation 0, control 89.5 (`rendered/`) |
| `check.mjs --blind` | exit 1, failing on the control alone (0.6 < 40) (`blind/`) |

CPU ms (CPU timing on, contended): p50 0.045, 0.05 and 0.05; p95 0.055, 0.065 and 0.07 (low MSAA,
full MSAA, low no-MSAA). GPU p95 reached three times its median, so GPU ms are not comparable to
WEB3D.md's table.

**`gates.sh` does not gate the blind run honestly.** `step_fails` accepts any non-zero exit. A blind
run whose port is taken exits 1 at `listen`, before any browser starts, and would count as held
(`gates/blind-port-taken.txt`). So would Chrome failing to launch, or a probe failing. Fix: give
`check.mjs --blind` an exit code of its own (for example 3) for "only the control failed", and make
`gates.sh` require that code.

## F1: the realm check follows the wrapper, not the document (`results/realm.json`, `results/observe.json`)

`canvasKind` (`js/web3d.js:311-324`) asks whether the canvas's prototype chain holds this realm's
`HTMLCanvasElement.prototype`. In Chrome, a node's wrapper keeps the realm that first wrapped it:

- **Adoption keeps it.** A canvas made by a same-origin frame's document and appended to the top
  document has the top as its owner document, and the frame's prototype (A1, A4 `adoptNode`).
- **First touch decides it.** The top's parser makes `<canvas id="mine">` in the top document. A
  same-origin frame's `MutationObserver` sees it added before the top's script touches it. The
  canvas then has the frame's prototype (`observe.html`). The control canvas, which no observer saw,
  is accepted.

So, with no prototype rewriting:

- **A frame's copy drives a canvas of the top document** (A1, A2, `observe`). That is exactly the
  case WEB3D.md lines 191-192 says is refused. Then the frame is removed (its renderer already
  disposed), and the canvas stays in the top document:
  - the next loss is not cancelled (`prevented: false`);
  - `restoreContext()` raises "context restoration not allowed";
  - the context is lost for good.

  The control, with the frame kept, is cancelled and restored, and each hook runs once. This is
  F1/F3's failure, inside the narrowed model.
- **The same happens with a popup's copy** on a popup-made canvas adopted into the opener (A9).
- **The top's own copy refuses canvases of its own document** with "belongs to another realm
  (another window's document…)": the adopted canvas, both before and after the frame is gone
  (A1, A2), and its own parser-made canvas (`observe`).
- **The top's copy accepts a canvas it made and then appended to a frame's or popup's document**
  (A3, A9).

False claims:

- WEB3D.md 188-196: "an element's realm is its document's window … the prototype chain … is where
  an element of another document shows its realm";
- WEB3D.md 281-283, and `js/web3d.js` 622-625: the listeners "run as long as the canvas's document
  or worker does";
- the `FOREIGN` message.

**Fix.** For an element, compare `Node.prototype.ownerDocument`'s getter (taken at load) with
`globalThis.document`. Keep the prototype check for `OffscreenCanvas`. Say that moving a driven
canvas to another document is unsupported, or recheck the owner document in `frame()` (one getter
call, no allocation). Add probes for adoption in both directions, first touch, and "frame removed,
then a loss".

What held: `importNode`, `createElement` through another document, customised built-ins and
subclasses (A5, A5b), `about:blank` and `srcdoc` frames (A7), workers and `OffscreenCanvas`
transfers both ways (A10). `Object.setPrototypeOf` gets through (A6), which the docs put out of scope.

## F2: two homes in one realm (`results/dd.json`)

`HOME` is worked out when each copy loads. Take a frame at `b.a.localhost` under a top at
`a.localhost`, both sending `Origin-Agent-Cluster: ?0`:

1. The frame loads copy 1. The top is out of reach, so copy 1's home is the frame.
2. The frame sets `document.domain`.
3. The frame loads copy 2 under another URL. Its home is the top.

The two copies now keep separate `OWNERS` maps. Both creates on one canvas of the frame succeed and
share one context. Copy 1 then draws black, and after copy 2 has drawn, copy 2's colour, with
`getError` 0.

This breaks WEB3D.md 196-198 ("copies … loaded under different URLs share the canvases' owners").
Contrived (it needs the deprecated `document.domain`), but inside the model.

**Fix.** Keep the owners, contexts and controllers on `globalThis`. Only one realm drives a canvas
now, so nothing needs them shared. Leave only the serial counter on `HOME`.

## F3: the reset leaves a page's drawing-buffer size (`results/reset.json`, S5)

On a page-made alpha context, `drawingBufferStorage(SRGB8_ALPHA8, 40, 30)` on a 300x150 canvas leaves
a 40x30 buffer. It is still 40x30 after `createRenderer({ width: 300, height: 150 })`, and after
`resize(300, 150)`. Chrome does not reallocate when the canvas is set to the size it already has.
web3d draws correctly at 40x30, but not at the size it was asked for, and nothing says so. Neither
of the reset's lists ("set" and "left alone") mentions this.

**Fix.** On alpha contexts, store `RGBA8` again when the buffer's size differs from the canvas's (as
clamped), or say so in the docs.

The rest of the reset holds:

- without alpha, `drawingBufferStorage` raises `INVALID_OPERATION` "alpha is required" (S3);
- after the reset to `RGBA8`, MSAA stays at 4 samples (S4);
- `preserveDrawingBuffer` contexts work (S6);
- after create, the test draw leaves the viewport at 1x1, no framebuffer, no texture and no error,
  and the first frame sets the viewport (S7);
- `OES_draw_buffers_indexed` mask, blend and equation left on buffer 0 draw as on a new context (S8);
- a page that never enabled `WEBGL_polygon_mode` gets no warning (`nowarn`).

## F4: false or stale claims (with `doc-check/README.md`, checked again where marked)

- WEB3D.md 225-226 and `gl.js` 362-363: three r180 "sets both [colour spaces] on every context". It
  sets them only in the `outputColorSpace` setter (`WebGLRenderer.js` 3399-3405). Checked again.
- WEB3D.md 244-246 and `renderer.rs` 911: line width, "which no draw uses". The polygon-mode test
  draws lines with the page's line width.
- WEB3D.md 532: `state`'s "23 states draw right". 22 draw right, and `blend_func_extended` is
  skipped (checked again).
- `critic10-rerun/README.md`:
  - "serials 560139 and 585108": `before-fix.json` has 33979, 7064 and 412917 (checked again);
  - `_put` "still at V8's middle tier": in Chrome 156, status 41 is TurboFan and 25 is Maglev,
    read from functions optimised by each tier (`handle-boxing/runs-40s.txt`), so `_put` is at the
    top tier;
  - "a run with TurboFan inlining off": no file records it.
- `check.mjs` 64-65 says 20.8 sampled bytes (22 elsewhere); `cpu-timing-allocation/README.md` says
  "the documents quote 20.8".
- `probe.js` 1314-1315: contexts released "so Chrome's limit on live contexts drops none of the
  others". This run logged 18 "Too many active WebGL contexts. Oldest context will be lost."
- `web3d.js` 1113-1114: "the flag the context-lost event sets". It is `isContextLost()` or the
  controller's loss count.
- `gl.js` 312: "a cross-origin image … throws". `<img>` is refused before upload now.
- `web3d.js` 15, `web3d.d.ts` 23, `slots.rs` 14: "the highest same-origin ancestor". The walk stops
  at the first parent it cannot reach.
- Smaller items in `doc-check/README.md`: "only unbound", the handle error "says which", "gates.sh
  runs every gate above", unpack parameters measured on 3 of the 5 source kinds, `demo.js` 7,
  `web.rs` 5-6, XR's "no listener" counted on one canvas only.

## Notes

- **Polygon-mode test and line width.** The test draw reads "filled" if a line of the page's width
  reaches the 1x1 pixel's centre. Its edges are 2 pixels from that centre, so a width of 5 or more
  would be misread as fill, and web3d would draw wireframe. Metal and SwiftShader clamp lines to
  [1, 1], so this cannot be shown here (`reset-angle-*.json`). A desktop GL driver with wide lines
  could hit it. Fix: `gl.lineWidth(1)` before the test.
- **Handle boxing** (`handle-boxing/runs-40s.txt`, the same method as `critic10-rerun/cpu-timing/`).
  Reproduced. Sampled bytes a frame for one `setTransform`, by where the handle is kept:

  | Where the handle is kept | Bytes a frame |
  | --- | --- |
  | an Array of numbers | 12.6 |
  | an object property | 12.2 |
  | a `Float64Array` | 11.9 |
  | a mixed Array | 0 |
  | a `Map` value | 0 |
  | a constant | 0 |

  Sixty handles read from an Array cost 748 bytes a frame, with `tick`, `setTransform` and `_put` all
  at TurboFan, so the cost is not a warm-up artifact. The docs state it honestly but too narrowly:
  the advice "variables or constants" does not fit a loop over objects. For phase 2, per-frame calls
  should not take a 2^30+ number per object. Either take handle objects (passing an object reference
  allocates nothing), or write per-frame data into a renderer-owned typed array indexed by a small
  slot number, with one call a frame. Phase 3's bones need the second anyway.
- **Earlier critics, rerun** (`rerun/`, `rerun/rerun.log`). All 24 scripts ran, and none of about 640
  booleans differ from the eleventh round's (`tools/bool-diff.mjs`). The by-premise arguments for
  critics 1 to 8 hold. The intents of two were retested without the old internals, and both pass
  (`results/intent.json`):
  - `manyOnOneCanvas`: each hook once, a shared hook once, a new renderer draws;
  - G14: the renderer limit refuses cleanly, and the canvas stays free at its size.

  The ninth's N7b and N11, and the tenth's `storageFloat` and `xtop`, hold by premise. The ninth's
  N10, N10c and N10e, and the tenth's `notop`, hold only literally. Their intent, a frame's copy on
  a canvas of the top document, still fails (F1).
- **Scope.** One realm per canvas is the right scope for a game drawn from its own top-level page,
  and also for one embedded in a learning platform's iframe, where web3d loads in the frame with the
  frame's canvas. Only the check needs to change.
- **Phase 1's bar** (WEB3D.md's plan row) is met apart from F1. Its own open item: dolores was
  measured with an earlier load and earlier shaders ("Re-measure it").

## Files

- `tools/`: server, driver and `bool-diff.mjs`.
- `site/`: the probe pages (`realm`, `dd`, `observe`, `reset`, `nowarn`, `intent`, and `worker.js`).
- `results/`: the probe pages' outputs.
- `gates/`, `rerun/`, `handle-boxing/` and `doc-check/`, as above.

Run a probe page with:

```
PLAYWRIGHT_CORE=.../playwright-core/index.mjs node tools/run.mjs <port> <page>.html [--host a.localhost]
```
