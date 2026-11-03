# The tenth critic of web3d phase 1 (2026-11-03)

loadngo `web3d-phase1` at `d924345f`. Chrome 156.0.8078.80 headless (`channel: 'chrome'`, ANGLE over
Metal, Apple M4 Pro); rustc 1.99.0, wasm-opt 133. The Mac's GPU was shared with another session
taking look captures (not timings), so the GPU times here are indicative only.

**Verdict: FAIL.** Three correctness bugs and some false claims. The gates pass.

## Gates

| Gate | Result | File |
| --- | --- | --- |
| `cargo fmt --check`, clippy as CI runs it (macOS row, and the wasm32 step) | pass | `gates-rust.txt` |
| `cargo test -p loadngo-web3d`, debug and release | 75 of 75 | `gates-rust.txt` |
| `build.sh` twice, then from an empty target directory | byte-identical; no local path (`strings`, `grep -a`); 58 imports, 28 function exports; sizes as the doc says | `builds.txt` |
| `three-reference.mjs --check` | 20 lines verbatim; output matches | `builds.txt` |
| `check.mjs` | pass: 72 of 72 probes, allocation 0.16 B a frame, control 89.27 | `check/` |
| `check.mjs --blind` | fails, as it must (control 0.6) | `blind/` |
| Every earlier critic's probes (`rerun-critics.sh`, a copy of the ninth rerun's script) | every script exits 0; each FAIL line is the same as in `critic9-rerun/earlier/`, the by-premise ones the doc names (fourth X8; fifth X8, K1, E1; sixth G1, G8a, G12, G14; seventh LC5; eighth E4, E16; ninth N7b, N11); no page errors | `earlier/`, `run9*.json`, `rerun-critics.log` |

CPU ms, fresh (`check/rendered-check.json`; CPU timing on; the machine was not quiet): p50 0.05,
0.055 and 0.055 ms, p95 0.175, 0.165 and 0.15 ms (1024x576 MSAA, 1366x768 MSAA, 1024x576 no MSAA).
That is below the doc's 0.12 and 0.165 ms p50 for the MSAA rows; those look like a loaded run, not a
regression. GPU p50 0.86, 1.47 and 0.65 ms, with p95 6 to 8 ms from the other session's GPU work.

## Findings (most severe first)

### F1. Under a cross-origin top window, same-origin frames do not share web3d's state: two live renderers on one canvas, and one draws the other's colours (FAIL)

- `js/web3d.js:327-338`: `HOME` is `top` when that is reachable, else the copy's own window. A page
  embedded in a cross-origin frame (an LMS page, a portal) with a same-origin child gets two homes:
  two owner maps, two controller maps, two serial counters.
- Probe `run10-xtop.json` (`site/xtop.html`, `nest-host.*`): a frame of origin A under a top of
  origin B makes renderer *a* on its canvas. Its same-origin child frame's copy then makes renderer
  *b* on the same canvas, and it is **not refused**. *b* draws red; *a*'s next `frame()` draws red
  too, not its own green, with `getError` 0. The two state caches share one context. The two
  serials come from separate random counters (24540, 905924).
- False claims:
  - `docs/WEB3D.md:91` and `:615` ("One renderer per canvas: a second is refused while the first is live").
  - `:161-163` ("Every copy of the module in a window and its same-origin frames shares one counter").
  - `js/web3d.js:13-15` and `:320-325`, `js/web3d.d.ts:21-23` and `:125`, `src/slots.rs:13`, `src/web.rs:486`.
- Fix: make `HOME` the topmost reachable same-origin window. Try `top` first, then walk `parent`
  while `w.parent[SHARED_KEY]` does not throw. Add a probe with a cross-origin top.

### F2. A page's colour-space and drawing-buffer state reaches web3d (FAIL)

`reset_context_state` and `reset_extension_state` leave three settings of the WebGL context alone.
three.js r180's own `WebGLRenderer` sets two of them on every context it uses
(`src/renderers/WebGLRenderer.js:3404-3405`). Each is measured against its own control, drawn the
same way (`run10-state2.json`, `site/state2.js`):

- **`gl.unpackColorSpace = 'display-p3'`:** every upload is colour-converted, although web3d sets
  `UNPACK_COLORSPACE_CONVERSION_WEBGL` to `NONE`. This holds for every source kind tried: a 2d canvas,
  an `ImageData`, and the `ImageBitmap` with `colorSpaceConversion: 'none'` that the docs recommend.
  The read-back is (194, 118, 84) against (206, 114, 76).
- **`gl.drawingBufferStorage(gl.SRGB8_ALPHA8, w, h)`:** the drawing buffer is sRGB, so web3d's
  sRGB-encoded output is encoded twice. A mid grey reads 192 against 134. The format survives
  web3d's own canvas resize (`drawingBufferFormat` is still `SRGB8_ALPHA8` afterwards).
- **`gl.drawingBufferColorSpace = 'display-p3'`:** the bytes are the same, but the screen shows them
  as P3. A screenshot reads (224, 104, 57) against (209, 111, 69).

False claims:
- `docs/WEB3D.md:91` ("what the page left bound or switched on does not reach web3d").
- `js/web3d.js:37-38` ("draws as a new one would") and `:487-489`.
- `js/web3d.d.ts:126-128`, `js/gl.js:285` ("reaches nothing"), `src/renderer.rs:892-893`.
- `js/gl.js:242-243` ("no colour management").

Fix: in the reset, set `gl.unpackColorSpace = 'srgb'` and `gl.drawingBufferColorSpace = 'srgb'`. If
`gl.drawingBufferFormat` is not `RGBA8`, call `gl.drawingBufferStorage(gl.RGBA8, w, h)`, or refuse
the context. Add the three to probe PS.

### F3. A frame's copy that ran before the top window's copy loaded: the top's copy never takes the listeners over (FAIL)

- `js/web3d.js:672`: the top's copy only sets `SHARED_STATE.install`. Listeners move only at a
  later create (`controllerFor`, `:625-628`), and the controllers live in a WeakMap that the top's
  copy cannot walk.
- Probe R3 (`run10-notop.json`, `site/frames.js`, real frames, nothing simulated):
  1. A same-origin frame's copy makes a renderer on a canvas of the top document, and another one
     it disposes.
  2. Then the top window imports web3d.js, so its copy is loaded.
  3. Then the frame is removed, and both contexts are lost before any create.
  4. Neither loss is cancelled (`defaultPrevented` false). `onContextLost` and `onContextRestored`
     never run, and `restoreContext()` is refused.
  5. The next create, from the top's copy, says `LOST_UNWATCHED`.
- False claims:
  - `docs/WEB3D.md:263-270` ("the listeners are made by the top window's copy of the module once one
    has loaded"; "One case is left: if no copy runs in the top window ...").
  - `docs/WEB3D.md:611-614` ("in one case: no copy of web3d runs in the top window").
  - `js/web3d.js:33-35` and `:609-615`, `js/web3d.d.ts:54`.
- This is a page that imports web3d lazily in the top and eagerly in a frame. The RF probe cannot
  see it: its top copy (`probe.js`) is always loaded before any frame's create.
- Fix: keep the canvases a controller watches in an iterable registry on the shared state (a Set
  of WeakRefs). When the top's copy loads, reinstall every live canvas's listeners from it. Or
  document this as a second case.

### F4. The reasons given for leaving `WEBGL_polygon_mode` out are false (FAIL)

Measured in `run10-pm.json` and `run10-state2.json`:

- **"Chrome prints a console warning whenever that extension is enabled":** it printed once a page.
  The first `getExtension` on the page printed it. Two more calls on that context, and three more
  fresh contexts, printed nothing.
- **"Whether the page enabled it cannot be asked without enabling it":**
  - `gl.getParameter(0x0B40)` (`POLYGON_MODE_WEBGL`) returns 6913 (`LINE_WEBGL`), with no error and
    no warning, on a context where the page enabled the extension and set lines. Where it is not
    enabled, it returns null with `INVALID_ENUM` and one console warning.
  - A test draw tells line mode apart with no error and no warning at all: one large triangle into
    a 1x1 framebuffer. It is filled in `FILL` mode and empty in `LINE` mode.
- So the exclusion's effect, wireframe drawing (off-diagonal pixels black in `state2`), is fixable
  without any warning: at init, make the test draw; only if it is empty, take the extension (the
  page's object, already enabled, whose warning has already been printed), set `FILL_WEBGL`, and
  switch off `POLYGON_OFFSET_LINE_WEBGL`.
- False claims: `docs/WEB3D.md:199-201`, `js/gl.js:311-313`, `src/gl.rs:183`, and the ninth rerun's
  README, item 1.

### F5. Small false claims (FAIL, low)

- `docs/WEB3D.md:58`: "`js/gl.js` creates the WebGL2 context". `js/web3d.js:522` (`build`) creates
  it; `createGl` is handed it.
- `critic9-rerun/README.md:53-55` says the old code "already handled" `UNPACK_ROW_LENGTH`, a
  "regression guard". `994f9bc8`'s `gl.js` and `renderer.rs` never set it, and the case passes
  against both. It guards nothing: a row length of 1 for a 2-wide source has no visible effect
  here. Use a state that the old code visibly fails on, or drop the claim.
- `docs/WEB3D.md:392`: "Textures are `SRGB8_ALPHA8`". With `srgb: false` they are `RGBA8`
  (`src/renderer.rs:1234-1238`).

## Checked and holding

- **The state reset:** 23 more states drawn on the first frame, right, with `getError` 0
  (`run10-state.json`):
  - depth mask off over a depth buffer cleared to 0; a 1x1 viewport; front face CW; cull
    `FRONT_AND_BACK`; stencil mask and op with the test off; dither off;
  - `OES_draw_buffers_indexed`: `colorMaskiOES`, `blendEquationiOES`, `blendFunciOES`, and
    `enableiOES(BLEND)` (the non-indexed calls reset every buffer);
  - an integer generic value on the uv attribute; divisors on vertex array 0; page buffers on
    uniform bindings 0-3; a texture on every unit with unit 7 active;
  - transform feedback paused, both on the bound object and on an unbound one;
  - `TRANSFORM_FEEDBACK_PRIMITIVES_WRITTEN` and `ANY_SAMPLES_PASSED_CONSERVATIVE` queries left
    running; every clip distance on;
  - `CONSTANT_COLOR` blending; `UNPACK_ALIGNMENT` 8; `BROWSER_DEFAULT` colour conversion.
- **Not reachable here:** `EXT_blend_func_extended` is not offered by this Chrome (it offers
  `WEBGL_blend_func_extended`), and nor is `WEBGL_shader_pixel_local_storage`. web3d's shaders have
  no `flat` varyings, so `WEBGL_provoking_vertex` cannot matter.
- **Listeners, against real frames** (`run10-notop.json`; the top has no copy until R3):
  - R1, the documented case, behaves as documented.
  - R2: a frame navigated to another origin counts as gone, and the next create from another
    frame recovers the loss (hooks 1 and 1).
  - R3b: a frame's create after the top's copy loaded gets the top's listeners.
  - R4: nested same-origin frames, the outer one removed: recovered, one listener of each kind.
- **The back/forward cache** (`run10-bf.json`, a Chrome allowed to use it): the page came back
  from the cache (`persisted` true). The renderer still draws, and a loss and restore call each
  hook once.
- **The RF probe's simulation** (`delete st.install`) is fair for what it claims: R1 and R2 confirm
  (e) and (b) with real frames. It cannot reach F3, because its top copy always loads first.
- **N7b fails by its premise:** its own lost listener is added before web3d's, which `build` adds
  after an `await`, so it cannot see web3d's `preventDefault`. Its intent is met by N7b' and MS.
- **N11 is a documented limit.** It is stated in `docs/WEB3D.md`, `js/web3d.js` and
  `js/web3d.d.ts`, and probe NC checks it. The ninth critic itself named documenting as the fix.
  A nit: "no browser call tells ... without making one" overlooks `transferControlToOffscreen()`,
  which tells (it throws) but transfers the canvas when it does not.

## Notes

- `docs/WEB3D.md:465` ("the GPU was not shared") and `:467` (CPU "moves with the machine's other
  load") are inferences, not evidence. `:472` "The critics' scripts pass on this code": several fail
  by premise, as the table under it says.
- The 20.8 B figure (`cpu-timing-allocation/`) dates from the first commit, and its page calls a
  method that no longer exists. It is cited as current in the doc, `gl.js`, `web3d.js` and `.d.ts`.
- `gates.sh` never exits non-zero, and does not enforce "`--blind` must fail". Its header says
  "after the ninth critic's round". `gates.txt` names `994f9bc8` "plus uncommitted changes", and
  the ninth rerun's README says "uncommitted when this was written".
- dolores was measured before two shader changes and the present load; the doc says to re-measure.
- `build.sh`'s workspace remap has no effect: the module holds relative `web3d/src/*.rs` paths,
  which are harmless.

## Files

- `run10.mjs`: the driver, which serves two origins on 127.0.0.1 (ports 5481 and 5482). It runs
  `state`, `state2`, `pm`, `notop`, `xtop` and `bf`, writes `run10-*.json`, and stops its servers
  and browsers.
- `site/`: the probe pages.
- `builds.sh` and `builds.txt`; `gates-rust.txt`.
- `check/` and `blind/`: `check.mjs` and `--blind`.
- `rerun-critics.sh`: the ninth rerun's script, writing here. Its results are `earlier/`,
  `run9*.json` and `rerun-critics.log`.
