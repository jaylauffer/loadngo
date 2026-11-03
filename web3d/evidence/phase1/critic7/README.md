# Seventh critic, web3d phase 1 (2026-11-03), generic evidence

Judged: loadngo `web3d-phase1` at `ffe5f208`. Chrome 156.0.8078.80 headless (`channel: 'chrome'`,
ANGLE over Metal, Apple M4 Pro); rustc 1.99.0, wasm-opt 133. Other sessions used the GPU at the same
time, so GPU times here are not a capacity measure. Anything about the application this renderer is
built for is kept in that application's own repository. Verdict: **FAIL**.

## Gates (all rerun here)

| Gate | Result | File |
| --- | --- | --- |
| `cargo fmt --check` (workspace, and `-p loadngo-web3d`) | pass | `gates.txt` |
| clippy `-D warnings`: CI's macOS-row workspace command; `-p loadngo-web3d` on host, wasm32 (CI's step), aarch64-linux and windows-msvc, from an empty target dir | pass | `gates.txt` |
| `cargo test --locked -p loadngo-web3d`, debug and release, empty target dir | 74 pass each | `gates.txt` |
| `build.sh` twice in place, once from an empty target dir, once from a `git archive` copy at another path | `pkg/` byte-identical to the pkg found every time (sha256 of all four files); no local path in the wasm | `build.txt`, `builds.sh` |
| `three-reference.mjs --check` (three 0.180.0) | pass, 20 lines verbatim, the committed table matches | `three-reference-check.txt` |
| `demo/check.mjs` | exit 0: 64 draws, 320,000 triangles in every run; 64/64 probes; steady 0.09 sampled B/frame; control 90.0 | `check.log`, `check/` |
| `demo/check.mjs --blind` | exit 1, as it must: control 0.6 against 40 | `check-blind.log`, `blind/` |
| every earlier critic's scripts (`rerun-earlier.sh`) | see below | `earlier/`, `rerun-earlier.log` |

## Earlier critics' probes

- First 29/29 (and its loss and wasm-edge scripts), second 14/14 (the seven three.js r180 lighting
  cases within 2/255 with the stand-in; without it 8, 6, 4 and 3 levels, as WEB3D.md says) and its
  ABI probe, third 10/10.
- Fourth: X1-X7 pass. Fifth: C1, C2, H5, R1, R2, T1, T2, L5, Y5, B1 pass. Sixth: G2-G7, G8b, G8c,
  G9a-c, G10, G11, G13, G16 pass.
- Fail by premise, and the builder's explanations (`../critic4-rerun/`, `../critic5-rerun/`,
  `../critic6-rerun/`) are right for every one. Each intent re-tested against the present design
  (`run7.json`):
  - X8 (fourth, fifth): `o + 2^-30` is `o`. H1: at the largest serial every fraction a double holds
    there (±1/2, ±1/4, ±1/8, 3/8, 0.1, 0.9) is refused by the wrapper, and 11 bad numbers straight into
    the export return 1; the object stays drawn.
  - K1 (fifth): no 32-renderer limit. G13 passes; S3 checks the real limit.
  - E1 (fifth): rerun, 134,213,633 handles (1 + 65,534 x 2,048), then "too many objects"; first,
    middle, last refused; getError 0.
  - G1 (sixth): decodes with the old shifts. H1 decodes the new layout exactly (serial 1048575,
    kind 4, generation 0, index 1).
  - G12 (sixth): 2,048 generations now; its own counts show 2,048 a slot, none twice. H2: one slot
    through all 2,048, retired, its first and last refused; 65,535 live objects, then a clean refusal;
    a freed slot comes back at generation 1.
  - G8a (sixth): an `<img>` is refused now; its other sources upload or are refused with getError 0.
  - G14 (sixth): the counter goes round now. S3: 2^20-1, then 1; refused once 2^20-1 are made, with the
    canvas size kept and the canvas usable.

## This critic's probes (`run7.mjs`, `probe-site/`; `run7.json`, `run7.txt`)

`pass` means the behaviour is right or the documented claim holds.

| Probe | Result | What it shows |
| --- | --- | --- |
| LC1 | **FAIL** | The LOST message's recovery, done as written ("dispose this renderer, then create a new one once the context is restored (onContextRestored)"): dispose in `onContextLost`, then wait. `onContextRestored` never fires (0 calls); the canvas's own restored event does (1). |
| LC2 | **FAIL** | A lost renderer, left beside a new one after the restore (which `createRenderer` allows), resizes the new renderer's canvas: `old.resize(8, 8)` does not throw, and the canvas goes from 32x32 to 8x8 under the new renderer. |
| LC3 | pass | A loss whose event arrives while the module instantiates: "the context is lost", restorable, the next create draws, getError 0. |
| LC3b | note | In LC3 the page's `onContextLost` hook runs once, for a create that then fails. |
| LC4 | pass | A loss during `web3d_init` (in `program_check`): "the context is lost", restorable, next create draws. The console still logs "shader program 2 failed:" with an empty log. |
| LC5 | pass | Lost, then disposed in the same task: the one-shot listener prevents the default once, the restore works, a new renderer draws; a later loss is not prevented. |
| LC6 | pass | Dispose while lost, restore, new renderer draws, getError 0. |
| LC7 | **FAIL** | Right after `loseContext()`, before the event: `frame()` returns **true** while `isContextLost()` is true and creates throw "lost". Reading `r.lost` latches the flag, after which `frame()` returns false. |
| LC8 | pass | After a loss and restore, a new renderer's first texture uploads. |
| D1 | pass | A page context without depth is refused for depth, twice, size kept. With antialias also wrong, the antialias message comes first and depth only on the second try. |
| D2 | pass | A page context with stencil, alpha, unpremultiplied alpha and a preserved buffer is accepted and draws near over far. |
| T1 | **FAIL** | A page-made context with one pending INVALID_ENUM: the first good ImageBitmap is refused, "the browser refused the texture upload (the console has the reason: a cross-origin image, a closed ImageBitmap or a lost context) ... raised WebGL error 0x500", and the page's error is gone (getError 0 after). The second texture works. |
| T2 | **FAIL** | Three pending errors (through `r.gl`): three good textures in a row are refused, citing 0x500, 0x501, 0x502. |
| T3 | pass, with a note | A `bitmaprenderer` canvas reports 300x150 for an 8x8 bitmap (OffscreenCanvas: 64x32): the upload raises INVALID_OPERATION and the texture is refused, so the size was not "exactly knowable"; only the post-upload check saves it, with the wrong reason. Placeholder canvases report their OffscreenCanvas's size and fill. |
| T4 | pass, with a note | A VideoFrame with an offset visible rectangle uploads exactly that region. A frame rotated 90 degrees (visible 4x2, display 2x4) raises INVALID_VALUE and is refused, again only by the post-upload check. |
| T5 | pass | An `<img>` from another realm is refused ("decode it first"); a detached ImageData, an SVG `<image>`, a transferred OffscreenCanvas are refused; getError 0. |
| S1 | pass | A same-origin frame shares the top window's counter (our 1007859, the frame's 1007860); a frame nested in a cross-origin frame, back at this origin, shares it too (1007861). |
| S2 | pass | A cross-origin frame (453337) and a module worker on an OffscreenCanvas (327276, draws) keep their own counters; all four foreign handles are refused by our renderer, which still draws. The page's first serial was 1007840 (random start). |
| S3 | pass | Goes round from 2^20-1 to 1; refused at 2^20-1 made ("this page has made 1048575 renderers"), size kept, canvas usable. |
| H1, H2 | pass | As above. |
| W1 | pass, with a note | Another realm's Module, URL, Response, Uint8Array and ArrayBuffer all compile and draw. A DataView passes web3d's check ("bytes") but V8's `WebAssembly.compile` refuses it with its own message. |
| W2 | pass | An object with `toString` (not called), a String object, a Proxy over a Response, a Promise, a Blob, a symbol and a number are refused with web3d's message. |

## Findings, most severe first

1. **The documented recovery from a context loss never gets its signal** (`js/gl.js:340-342`;
   `js/web3d.js:30-32`, `:48-49`; `docs/WEB3D.md:198`; `js/web3d.d.ts:109-110`; LC1). Every text says
   dispose, then create a new renderer after `onContextRestored`. `dispose()` removes the restored
   listener, so a page that disposes first, as the LOST message says, waits for a hook that never
   comes, and the canvas stays blank. K2 passes only because it waits for the hook before disposing.
   Fix: when `dispose()` runs on a lost renderer, keep a one-shot restored listener that calls the
   hook (as the lost one already does), or reword all four places: "wait for onContextRestored, then
   dispose and create".
2. **The post-upload `getError` takes any pending error as the upload's** (`js/gl.js:251-257`;
   T1, T2). With a page-made context (supported: "whoever made it"), an error the page left makes the
   first good texture fail, with a reason that names neither, and the page's error is consumed. Each
   pending error costs one texture. Fix: drain `getError` (bounded) just before `texStorage2D`,
   reporting what was pending as pending, then check after the upload. Update the reason in
   `src/renderer.rs:292-294`, which lists only "a cross-origin image, a closed ImageBitmap or a lost
   context".
3. **A lost renderer can still resize the canvas a new renderer owns** (`js/web3d.js:552-557`,
   `:618-638`; LC2). `createRenderer` lets a new renderer start beside a lost one that has not been
   disposed (`owner && !owner.lost`), and probe K tests exactly that order, but `resize` on the old
   one still sets the canvas's width and height. Fix: refuse `resize` once lost, or when the
   renderer no longer owns the canvas.
4. **`frame()` draws into a lost context until the event, and what it returns depends on whether
   `r.lost` was read** (`js/web3d.js:949`; `docs/WEB3D.md:196`, "Once the WebGL context is lost:
   frame() returns false"; `js/web3d.d.ts:144`; LC7). Fix: `if (this._shim.lost()) return false;`.
5. **Claims in live sources that are false now:**
   - `js/web3d.js:711-716`: `createTexture`'s comment still lists "an image" and "a video", which are
     refused.
   - `src/renderer.rs:1124-1126`: "(an `ImageBitmap`, `HTMLImageElement` or canvas)".
   - `demo/probe-frame.html:9`, "its own copy of web3d.js and its own serials", and `demo/probe.js:982`
     and `:1003` ("has its own serials, starting at random"; "each realm starts at random"): a
     same-origin frame shares the top window's counter (S1). R passes because the serials differ,
     for the opposite reason.
   - `docs/WEB3D.md:93` and `js/web3d.d.ts:75`, `js/web3d.js:181`: "sources whose upload size is
     exactly knowable" is not so for a `bitmaprenderer` canvas or a rotated VideoFrame (T3, T4); they
     are refused only by the post-upload check.
   - `docs/WEB3D.md:8`: "has been through five critic rounds"; six had been.
   - `docs/WEB3D.md:381`: "each p95 is within 2% of its median": 0.859 against 0.842 is 2.0%, and 0.579
     against 0.566 is 2.3% (`rendered-check.json`).

## Notes

- LC3b: `onContextLost` can run for a create that then fails (the event arrived during
  instantiation). A page hook that disposes "the renderer" has none yet.
- LC4: during a loss in `web3d_init` the console logs "shader program 2 failed:" with an empty log
  (`js/gl.js:132-136`), though the error thrown is right.
- D1: a page context that lacks depth and has the wrong antialias needs two failed creates to learn
  both; name every requirement in one message.
- W1: `BufferSource` in `web3d.d.ts:42` includes DataView, which V8 refuses.
- The `wasm` brand checks hold across realms, and nothing outside the documented types gets through
  (W1, W2). The sixth critic's two notes on this are fixed.
- The sixth critic's C1-C5 are fixed (G9a-c, G8b, G8c, G6 pass; K2, K3, R, H7 exist; the two stale
  comments are gone; "By hand" lists all six scripts).
- 28 committed run records under `web3d/evidence/` hold `/Users/<name>/...` paths (all inside
  loadngo). Not the application's, but a user name in a public repository.
- `docs/WEB3D.md:427-428` cites `critic4-rerun/reproducible.txt` for the reproducible build, from an
  earlier pkg; it still holds (`build.txt` here, including a build at another path).
- The dolores figures are for the earlier load and shaders, as WEB3D.md says. Phase 1 still has no
  measurement of its present code on the hardware it is for.

## Is phase 1 done?

Not yet. Every part of the bar exists and every gate passes, and the boundary attacks of the last
two rounds now hold. What is left is on the context-loss path the docs prescribe (finding 1), and in
this round's new upload check (finding 2), plus two smaller lost-renderer bugs and six false claims.
All are small fixes.
