# Critic 11: docs and comments checked against their sources (loadngo web3d-phase1, 33df5d4a)

Read-only check; no build, server or browser. Counts were taken from the committed `pkg/` (its JS
files equal `js/`), the committed evidence files, and three.js 0.180.0's source.

## False or stale

1. `docs/WEB3D.md:225-226` and `js/gl.js:362-363`: "three.js r180's WebGLRenderer sets both
   [colour spaces] on every context it uses". It sets them only in the `outputColorSpace` setter
   (`src/renderers/WebGLRenderer.js:3404-3405`); the constructor assigns `this._outputColorSpace`
   directly (line 276), so a context gets them only when the app assigns `outputColorSpace`.
2. `docs/WEB3D.md:250-251` and `src/renderer.rs:912`: the page's objects "are kept, not deleted:
   only unbound". The reset leaves textures bound on every unit, `ARRAY_BUFFER`, the copy and
   transform-feedback buffer bindings, uniform-buffer bindings, and the current program (unbound only
   when the polygon-mode test draw runs).
3. `docs/WEB3D.md:172-173`: another renderer's handle "is refused with an error that says which".
   The message (`src/renderer.rs:304-307`) says "destroyed, never created, or another renderer's".
4. `docs/WEB3D.md:244-246` and `src/renderer.rs:911`: "the blend colour and line width, which no
   draw uses". The polygon-mode test draw (`js/gl.js:106-146`) rasterises lines when the page left
   LINE mode, so the page's line width reaches it.
5. `docs/WEB3D.md:246-248`: the unpack row length, image height, skip-images and alignment "do not
   change an upload from the image sources web3d takes". `critic10-rerun/unpack-params/` measured
   only `ImageBitmap`, `ImageData` and a 2d canvas (21 cases), not `VideoFrame`, `OffscreenCanvas` or
   a WebGL canvas.
6. `docs/WEB3D.md:532`: "`state`'s 23 states draw right". 22 draw right; the 23rd
   (`EXT_blend_func_extended`) is skipped: no such extension here (`critic10-rerun/run10-state.json`).
7. `docs/WEB3D.md:645`: `gates.sh` "runs every gate above". It does not run the critics' scripts
   listed just above it (`rerun-critics.sh` does).
8. `demo/check.mjs:64-65`: "20.8 sampled bytes a frame, see Renderer.setCpuTiming". `setCpuTiming`
   (`js/web3d.js:1133`) and every other source now say 22.
9. `evidence/phase1/cpu-timing-allocation/README.md`, last line: "The renderer's documents quote
   the 20.8 measured here". They quote 22.0 from `critic10-rerun/cpu-timing/`.
10. `evidence/phase1/critic10-rerun/README.md`, "The control": "serials 560139 and 585108".
    `before-fix.json` holds `childSerial` 33979 and `ourSerial` 7064.
11. `evidence/phase1/critic10-rerun/README.md`, CPU timer: "A run with TurboFan inlining off charged
    it to the tick too". No file in `cpu-timing/` records that run.
12. `evidence/phase1/critic10-rerun/README.md`: `runs-120s.txt` shows "`_put` still at V8's middle
    tier". `_put`'s status there is 41; every other method's is 25, which
    `cpu-timing-allocation/README.md` reads as Maglev. So 41 is not Maglev by the rounds' own
    reading; it is most likely TurboFan.
13. `demo/demo.js:7`: "Nothing on the demo's per-frame path allocates". That holds only with
    `cputime=0`, but CPU timing is on by default (`demo.js:35`). The check's timing window reads 165.1
    sampled bytes a frame from `demo.js` (`rendered-check.json`).
14. `demo/index.html` (query-parameter comment): lists scale, aa, knots, tubular, radial and still. It
    leaves out `cputime`, `hud` and `arrays`.
15. `src/web.rs:5-6`: "a frame allocates nothing on either side". With CPU timing on, the shim's
    `now` readings are boxed (22 bytes a frame).
16. `js/web3d.js:1113-1114`: `frame()` returns false once lost, "(the flag the context-lost event
    sets)". The flag is `gl.isContextLost()` or the controller's loss count (`js/gl.js:71-74`), which
    is set before the event arrives (line 1122 says so).
17. `js/gl.js:312`: "A cross-origin image ... throws". An `<img>` is refused before any upload now;
    the case that throws is a tainted canvas or a cross-origin `ImageBitmap`.
18. `js/web3d.js:15`, `js/web3d.d.ts:23` and `src/slots.rs:14` say "the highest same-origin
    ancestor a frame can reach". The code (`js/web3d.js:360`) stops at the first unreachable parent,
    so it is the top of the contiguous same-origin chain. `docs/WEB3D.md:163` states this correctly.
19. `demo/probe.js:1314-1315`: the probes release their contexts, "so Chrome's limit on live contexts
    drops none of the others". The passing run's probe console has 18 "Too many active WebGL
    contexts. Oldest context will be lost." warnings (`rendered-check.json`, `probes.messages`).
    Nothing records which contexts Chrome dropped.
20. `critic10-rerun/README.md` (XR) and `demo/probe.js:1736-1737`: "each refused canvas keeps no
    context and no listener (counted over the DevTools protocol)". Listeners are counted for one
    canvas only (`xr-top`). The frame's canvas is checked for a context only, and the two
    `OffscreenCanvas` refusals for neither.

Polish: `docs/WEB3D.md:97` writes `setAmbient(sky, ground, intensity)`, but the call takes 7
numbers. `js/web3d.d.ts` `MaterialOptions.baseColor` has two JSDoc blocks, and TypeScript keeps
only the second. The d.ts `Renderer` declares no constructor, so `new Renderer()` type-checks.
`docs/WEB3D.md:179` says every method throws after `dispose()`, but the getters (`cpuMs`, `gpuMs`,
`lost`, `gl`, `info`) do not.

## Checked and true

75 unit tests; 58 imports (module `web3d_gl`, all provided by `gl.js`) and 28 exported functions;
72 probes with unique ids; PS 36 states plus 2 controls; the sizes table (70,217/32,160,
21,026/7,089, 51,762/16,103); every Results-table number and the 2.88/1.53/2.30% figures; 16.67 ms
frames; clear (33, 36, 43); allocation 0 over 601 frames, control 87.4, blind 0.5, cold
258.9/28.7/2.1; handle figures 12.0/11.3/1.5/1.1, timer 22.0 and 20.8; quoted CPU figures from
`d924345f` (0.12, 0.165) and critic 10 (0.05-0.055); the dolores table and fit (2.32 + 0.0046,
R² 0.985); three.js lines 2501-2504, `BRDF_BlinnPhong`, `1/max(pow(d,decay),0.01)` and `smoothstep`;
20 transcribed lines; within 2/255 on 7 cases, and 8/4/3 levels without the stand-in; handle layout
and limits (2^50, 134,215,680 a table, 134,213,633 with one kept, 2^20-1 serials, about 2k in a
million); `MAX_MORPH_TARGETS` 256, `WEIGHT_SUM_TOLERANCE` 0.02, `MAX_DRAWING_BUFFER` 32,768; the
144/128/32-byte blocks; the precision list; frame steps 1-8 and the sort key; the
`reset_context_state` list against `WEB3D.md`; the CI paragraph and the exact wasm32 command; the
unsafe-code locations; build.sh as described; the wasm-bindgen evidence; the licence; critic rows 1
to 10 against the `earlier/`, `run9*` and `run10-*` files (pass and FAIL ids all match); every FAIL
line for critics 1 to 8 identical to the ninth rerun; `before-fix.json` 70 of 72, with the seven PS
values quoted.

## Browser claims to test

- `drawingBufferStorage` on an `alpha: false` context raises `INVALID_OPERATION`, so the format
  cannot change, and the format survives a canvas resize (`js/gl.js:365-366`, `WEB3D.md:229-230`).
- The prototype check shows an element's document's realm (`WEB3D.md:195-196`): does `adoptNode`
  or `appendChild` into another document leave the old prototype?
- What V8 status 25 and 41 mean in Chrome 156 (item 12).
- The polygon-mode test draw under a page `lineWidth` above 4, where the range allows it (item 4).
